//! P1-096: onboarding checklist API, end to end.
//!
//! Through the real router (`TestApp` oneshot) against the
//! migrated scratch database: a fresh user sees only the
//! ToS step done (register accepted the current versions);
//! each step flips exactly when its underlying event lands
//! — email verified over HTTP (the token comes from the
//! `verify-email` outbox row, the email_flow precedent),
//! first upload / first job seeded through the repos (both
//! creates are still 501 stubs until Phase 2, so the repo is
//! the only way to put the rows there), first API key minted
//! over HTTP (Pro-granted, the P1-055 plan gate); a config
//! bump reopens the ToS step until the re-accept lands; a
//! revoked key still counts (the step records the action
//! done once). The GET writes no audit row. The token comes
//! from a real login (register + login through the router),
//! never minted by hand.

mod common;

use axum::http::{Method, Request, StatusCode};
use chrono::{DateTime, Utc};
use common::{TestApp, TestFixtures};
use detag_api::services::verification::VERIFY_EMAIL_TEMPLATE;
use detag_core::{
    checksum::sha256_hex,
    password::{hash_password, Argon2Params},
    repos::{
        audit_logs, email_outbox,
        jobs::{self, NewJob},
        uploads::{self, NewUpload},
        users,
    },
};
use serde_json::{json, Value};
use uuid::Uuid;

/// The request id: a valid UUID, echoed by the middleware.
const REQUEST_ID: &str = "123e4567-e89b-42d3-a456-426614174104";

/// The password every test user registers with.
const PASSWORD: &str = "correct horse battery staple";

/// App with both current versions at v1 (the default).
async fn onboarding_app() -> TestApp {
    let fixtures = TestFixtures::from_env().await;
    let mut settings = fixtures.settings();
    settings.disposable_email_domains = String::new();
    settings.tos_current_version = "v1".to_string();
    settings.aup_current_version = "v1".to_string();
    TestApp::spawn_with_fixtures_and_settings(fixtures, settings).await
}

/// App with both current versions bumped to v2 — the bumped
/// config a re-accept is tested against. A user seeded with
/// v1/v1 through the repo is exactly what a production bump
/// leaves behind: config changed, rows didn't.
async fn onboarding_app_v2() -> TestApp {
    let fixtures = TestFixtures::from_env().await;
    let mut settings = fixtures.settings();
    settings.disposable_email_domains = String::new();
    settings.tos_current_version = "v2".to_string();
    settings.aup_current_version = "v2".to_string();
    TestApp::spawn_with_fixtures_and_settings(fixtures, settings).await
}

fn unique_email() -> String {
    format!("onboarding-{}@example.com", Uuid::now_v7())
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

/// The raw verification token for the user's register mail
/// (the email_flow precedent: the outbox payload carries it).
async fn verify_token_for(app: &TestApp, email: &str) -> String {
    let mails = email_outbox::unsent_for_template(&app.pool, VERIFY_EMAIL_TEMPLATE)
        .await
        .expect("outbox read");
    let hash = sha256_hex(email.as_bytes());
    let ours: Vec<_> = mails
        .iter()
        .filter(|m| m.to_hash.as_deref() == Some(hash.as_str()))
        .collect();
    assert_eq!(ours.len(), 1, "exactly one verify mail, got {}", ours.len());
    ours[0].payload["token"]
        .as_str()
        .expect("payload carries the raw token")
        .to_string()
}

/// Verify the email over HTTP with the outbox token.
async fn verify_email(app: &TestApp, token: &str) {
    let res = app
        .request(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/auth/verify-email")
                .header("x-request-id", REQUEST_ID)
                .header("x-forwarded-for", unique_xff())
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    serde_json::to_vec(&json!({ "token": token })).unwrap(),
                ))
                .unwrap(),
        )
        .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());
}

/// Seed an upload for the user (the attestations helper —
/// upload-create is still a 501 stub until Phase 2, so the
/// repo is the only way to put the row there).
async fn seed_upload(app: &TestApp, user_id: Uuid) -> Uuid {
    uploads::create(
        &app.pool,
        &NewUpload {
            user_id,
            kind: "image",
            filename: "a.png",
            bytes: 100,
            sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            mime: "image/png",
            storage_key: "test/onboarding-seed",
            parts: 1,
        },
    )
    .await
    .expect("seed upload")
    .id
}

/// Seed a job for the user (the admin_abuse_queue helper — a
/// job's `upload_id` is nullable, so no upload is needed).
async fn seed_job(app: &TestApp, user_id: Uuid) -> Uuid {
    jobs::create(
        &app.pool,
        &NewJob {
            user_id,
            upload_id: None,
            kind: "image",
            mode: "inpaint",
            status: "queued",
            lane: "standard",
            params: json!({}),
            idempotency_key: Some(&format!("p096-{}", Uuid::now_v7())),
            key_id: None,
        },
    )
    .await
    .expect("seed job")
    .id
}

/// Mint an API key over HTTP (Pro-granted: key creation is
/// plan-gated since P1-055). Returns the key id.
async fn create_key(app: &TestApp, token: &str) -> Uuid {
    let res = app
        .request(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/api-keys")
                .header("x-request-id", REQUEST_ID)
                .header("x-forwarded-for", unique_xff())
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    serde_json::to_vec(&json!({ "name": "onboarding", "scopes": ["jobs:read"] }))
                        .unwrap(),
                ))
                .unwrap(),
        )
        .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.text());
    res.json()["id"].as_str().unwrap().parse().unwrap()
}

/// Revoke an API key over HTTP (204).
async fn revoke_key(app: &TestApp, token: &str, key_id: Uuid) {
    let res = app
        .request(
            Request::builder()
                .method(Method::DELETE)
                .uri(format!("/v1/api-keys/{key_id}"))
                .header("x-request-id", REQUEST_ID)
                .header("x-forwarded-for", unique_xff())
                .header("authorization", format!("Bearer {token}"))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());
}

fn get_onboarding(token: &str) -> Request<axum::body::Body> {
    Request::builder()
        .method(Method::GET)
        .uri("/v1/me/onboarding")
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff())
        .header("authorization", format!("Bearer {token}"))
        .body(axum::body::Body::empty())
        .unwrap()
}

async fn fetch_onboarding(app: &TestApp, token: &str) -> Value {
    let res = app.request(get_onboarding(token)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    res.json()
}

/// One step of the checklist, by id.
fn step<'a>(checklist: &'a Value, id: &str) -> &'a Value {
    checklist["steps"]
        .as_array()
        .expect("steps is an array")
        .iter()
        .find(|step| step["id"] == id)
        .unwrap_or_else(|| panic!("step {id} missing: {checklist}"))
}

/// A completed_at value parsed back to a timestamp (the
/// response carries RFC 3339; the DB carries microseconds —
/// the round-trip is exact).
fn completed_at(checklist: &Value, id: &str) -> Option<DateTime<Utc>> {
    step(checklist, id)["completed_at"]
        .as_str()
        .map(|s| s.parse().expect("completed_at parses"))
}

#[tokio::test]
async fn fresh_user_checklist() {
    let app = onboarding_app().await;
    let email = unique_email();
    let user_id = register_user(&app, &email).await;

    // No credential is 401 (the route-table probe covers it
    // too; here it proves the live handler enforces it).
    let res = app
        .request(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/me/onboarding")
                .header("x-request-id", REQUEST_ID)
                .header("x-forwarded-for", unique_xff())
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1001");

    let token = login_token(&app, &email).await;

    // The GET writes no audit row: count the user's rows
    // before and after.
    let before = audit_logs::list_for_target(&app.pool, "user", &user_id.to_string())
        .await
        .expect("list audits")
        .len();

    let checklist = fetch_onboarding(&app, &token).await;
    assert_eq!(checklist["total_count"], 5, "{checklist}");
    assert_eq!(checklist["completed_count"], 1, "{checklist}");
    assert_eq!(checklist["all_done"], false, "{checklist}");
    let ids: Vec<&str> = checklist["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        [
            "verify_email",
            "accept_tos",
            "first_upload",
            "first_job",
            "create_api_key"
        ],
        "{checklist}"
    );
    assert_eq!(step(&checklist, "verify_email")["completed"], false);
    assert!(step(&checklist, "verify_email")["completed_at"].is_null());
    // Only the ToS step is done: register accepted the
    // current versions, and its time is the account's
    // creation (register writes no consent audit row).
    assert_eq!(step(&checklist, "accept_tos")["completed"], true);
    let user = users::by_id(&app.pool, user_id)
        .await
        .unwrap()
        .expect("user row exists");
    assert_eq!(
        completed_at(&checklist, "accept_tos"),
        Some(user.created_at)
    );
    assert_eq!(step(&checklist, "first_upload")["completed"], false);
    assert_eq!(step(&checklist, "first_job")["completed"], false);
    let key = step(&checklist, "create_api_key");
    assert_eq!(key["completed"], false);
    assert_eq!(key["optional"], true);

    let after = audit_logs::list_for_target(&app.pool, "user", &user_id.to_string())
        .await
        .expect("list audits")
        .len();
    assert_eq!(after, before, "a read writes no audit row");
}

#[tokio::test]
async fn steps_transition_correctly() {
    let app = onboarding_app().await;
    let email = unique_email();
    let user_id = register_user(&app, &email).await;
    let token = login_token(&app, &email).await;

    // Email: unverified at register, verified over HTTP.
    let checklist = fetch_onboarding(&app, &token).await;
    assert_eq!(step(&checklist, "verify_email")["completed"], false);
    let raw = verify_token_for(&app, &email).await;
    verify_email(&app, &raw).await;
    let checklist = fetch_onboarding(&app, &token).await;
    assert_eq!(step(&checklist, "verify_email")["completed"], true);
    assert!(
        completed_at(&checklist, "verify_email").is_some(),
        "verified step carries its time: {checklist}"
    );
    assert_eq!(checklist["completed_count"], 2, "{checklist}");
    assert_eq!(checklist["all_done"], false, "{checklist}");

    // First upload: the seed flips the step with its time.
    seed_upload(&app, user_id).await;
    let checklist = fetch_onboarding(&app, &token).await;
    assert_eq!(step(&checklist, "first_upload")["completed"], true);
    assert!(
        completed_at(&checklist, "first_upload").is_some(),
        "upload step carries its time: {checklist}"
    );
    assert_eq!(checklist["completed_count"], 3, "{checklist}");

    // First job: same transition.
    seed_job(&app, user_id).await;
    let checklist = fetch_onboarding(&app, &token).await;
    assert_eq!(step(&checklist, "first_job")["completed"], true);
    assert!(
        completed_at(&checklist, "first_job").is_some(),
        "job step carries its time: {checklist}"
    );
    assert_eq!(checklist["completed_count"], 4, "{checklist}");
    // All required steps done — the optional key does not
    // block all_done.
    assert_eq!(checklist["all_done"], true, "{checklist}");

    // First API key, minted over HTTP (Pro-granted).
    app.grant_plan(user_id, "pro").await;
    create_key(&app, &token).await;
    let checklist = fetch_onboarding(&app, &token).await;
    assert_eq!(step(&checklist, "create_api_key")["completed"], true);
    assert!(
        completed_at(&checklist, "create_api_key").is_some(),
        "key step carries its time: {checklist}"
    );
    assert_eq!(checklist["completed_count"], 5, "{checklist}");
    assert_eq!(checklist["all_done"], true, "{checklist}");
}

#[tokio::test]
async fn bump_reopens_tos_step() {
    let app = onboarding_app_v2().await;
    let email = unique_email();
    let user_id = seed_v1_user(&app, &email).await;
    let token = login_token(&app, &email).await;

    // The bump reopened the step: incomplete, no time — even
    // though the row still says v1/v1.
    let checklist = fetch_onboarding(&app, &token).await;
    assert_eq!(step(&checklist, "accept_tos")["completed"], false);
    assert!(step(&checklist, "accept_tos")["completed_at"].is_null());
    assert_eq!(checklist["all_done"], false, "{checklist}");

    // Re-accept the current versions over HTTP.
    let res = app
        .request(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/me/consent")
                .header("x-request-id", REQUEST_ID)
                .header("x-forwarded-for", unique_xff())
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {token}"))
                .body(axum::body::Body::from(
                    serde_json::to_vec(
                        &json!({"accept_tos_version": "v2", "accept_aup_version": "v2"}),
                    )
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());

    let checklist = fetch_onboarding(&app, &token).await;
    assert_eq!(step(&checklist, "accept_tos")["completed"], true);
    // The time is the re-accept's audit row, not the account's
    // creation (the row predates the bump).
    let rows = audit_logs::list_for_action(&app.pool, "user.consent_accepted")
        .await
        .expect("list audits");
    let row = rows
        .iter()
        .find(|row| row.target_id.as_deref() == Some(&user_id.to_string()))
        .expect("consent audit exists");
    assert_eq!(completed_at(&checklist, "accept_tos"), Some(row.created_at));
}

#[tokio::test]
async fn revoked_key_still_counts() {
    let app = onboarding_app().await;
    let email = unique_email();
    let user_id = register_user(&app, &email).await;
    let token = login_token(&app, &email).await;
    app.grant_plan(user_id, "pro").await;

    let key_id = create_key(&app, &token).await;
    let checklist = fetch_onboarding(&app, &token).await;
    assert_eq!(step(&checklist, "create_api_key")["completed"], true);

    // Revoke it: the step stays done — the action was done
    // once, and revocation is lifecycle, not un-completion.
    revoke_key(&app, &token, key_id).await;
    let checklist = fetch_onboarding(&app, &token).await;
    assert_eq!(
        step(&checklist, "create_api_key")["completed"],
        true,
        "{checklist}"
    );
    assert!(
        completed_at(&checklist, "create_api_key").is_some(),
        "{checklist}"
    );
}
