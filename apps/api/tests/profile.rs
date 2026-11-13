//! P1-005: the profile endpoints, end to end.
//!
//! Through the real router (`TestApp` oneshot) against the
//! migrated scratch database: the auth rejections (no
//! credential, malformed JWT, expired JWT, valid JWT for an
//! unknown user — 401 PF-1001/1010/1011), the GET happy path
//! (Free plan + zero usage + the seeded-usage snapshot), and
//! the PATCH rules (display name set/trim/clear/invalid,
//! email-change intent set/clear, unchanged/disposable/
//! duplicate rejections, no-op PATCH). The token comes from a
//! real login (register + login through the router), never
//! minted by hand — except the expired/unknown-user cases,
//! which mint with the app's own secret to prove the extractor
//! rejects them.

mod common;

use axum::http::{Method, Request, StatusCode};
use common::{TestApp, TestFixtures};
use detag_core::{
    jwt::issue_access_token,
    repos::{audit_logs, users},
};
use serde_json::json;
use uuid::Uuid;

/// The request id: a valid UUID, echoed by the middleware.
const REQUEST_ID: &str = "123e4567-e89b-42d3-a456-426614174103";

/// The password every test user registers with.
const PASSWORD: &str = "correct horse battery staple";

async fn profile_app() -> TestApp {
    let fixtures = TestFixtures::from_env().await;
    let mut settings = fixtures.settings();
    // The disposable denylist is on for the disposable test;
    // the test users register with example.com addresses.
    settings.disposable_email_domains = "mailinator.com".to_string();
    settings.tos_current_version = "v1".to_string();
    TestApp::spawn_with_fixtures_and_settings(fixtures, settings).await
}

fn unique_email() -> String {
    format!("profile-{}@example.com", Uuid::now_v7())
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

/// A per-test XFF address: each test gets its own IP bucket,
/// so the tests never trip each other's rate limits (the
/// P1-003 lesson — the shared "unknown" bucket is one bucket
/// for the whole binary).
fn unique_xff() -> String {
    format!("203.0.113.{}", Uuid::now_v7().as_u128() % 250 + 1)
}

fn get_me(token: Option<&str>) -> Request<axum::body::Body> {
    let mut req = Request::builder()
        .method(Method::GET)
        .uri("/v1/me")
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff());
    if let Some(token) = token {
        req = req.header("authorization", format!("Bearer {token}"));
    }
    req.body(axum::body::Body::empty()).unwrap()
}

fn patch_me(token: Option<&str>, body: serde_json::Value) -> Request<axum::body::Body> {
    let mut req = Request::builder()
        .method(Method::PATCH)
        .uri("/v1/me")
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff())
        .header("content-type", "application/json");
    if let Some(token) = token {
        req = req.header("authorization", format!("Bearer {token}"));
    }
    req.body(axum::body::Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

#[tokio::test]
async fn get_me_without_auth_is_401() {
    let app = profile_app().await;
    let res = app.request(get_me(None)).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    let json = res.json();
    assert_eq!(json["code"], "PF-1001");
    assert_eq!(json["request_id"], REQUEST_ID);
}

#[tokio::test]
async fn get_me_with_malformed_jwt_is_401() {
    let app = profile_app().await;
    let res = app.request(get_me(Some("aaa.bbb.ccc"))).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1010");
}

#[tokio::test]
async fn get_me_with_expired_jwt_is_401() {
    let app = profile_app().await;
    let expired = issue_access_token(&app.settings.jwt_secret, Uuid::now_v7(), "user", -60)
        .expect("expired token issues");
    let res = app.request(get_me(Some(&expired))).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1011");
}

#[tokio::test]
async fn get_me_with_valid_jwt_for_unknown_user_is_401() {
    let app = profile_app().await;
    let token = issue_access_token(&app.settings.jwt_secret, Uuid::now_v7(), "user", 900)
        .expect("token issues");
    let res = app.request(get_me(Some(&token))).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1001");
}

#[tokio::test]
async fn get_me_happy_path_free_plan_zero_usage() {
    let app = profile_app().await;
    let email = unique_email();
    let user_id = register_user(&app, &email).await;
    let token = login_token(&app, &email).await;

    let res = app.request(get_me(Some(&token))).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let json = res.json();
    assert_eq!(json["id"], user_id.to_string());
    assert_eq!(json["email"], email);
    assert_eq!(json["email_verified"], false);
    assert!(json.get("display_name").is_none(), "{json}");
    assert!(json.get("pending_email").is_none(), "{json}");
    assert_eq!(json["role"], "user");
    assert_eq!(json["status"], "active");
    assert_eq!(json["plan"]["code"], "free");
    assert_eq!(json["plan"]["amount"], 0);
    assert_eq!(json["plan"]["currency"], "INR");
    // Free: 10 images/day, 0 video seconds/day (the seeded plan
    // entitlements); nothing used yet.
    assert_eq!(
        json["quota"]["daily_images"],
        json!({"limit": 10, "used": 0, "remaining": 10})
    );
    assert_eq!(
        json["quota"]["daily_video_seconds"],
        json!({"limit": 0, "used": 0, "remaining": 0})
    );
    // No password hash, ever.
    assert!(json.get("password_hash").is_none(), "{json}");
}

#[tokio::test]
async fn get_me_reflects_seeded_usage() {
    let app = profile_app().await;
    let email = unique_email();
    let user_id = register_user(&app, &email).await;
    let token = login_token(&app, &email).await;

    sqlx::query(
        "INSERT INTO usage_counters (user_id, day, images_used, video_seconds_used)
         VALUES ($1, CURRENT_DATE, 3, 0)",
    )
    .bind(user_id)
    .execute(&app.pool)
    .await
    .expect("seed usage");

    let res = app.request(get_me(Some(&token))).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let json = res.json();
    assert_eq!(
        json["quota"]["daily_images"],
        json!({"limit": 10, "used": 3, "remaining": 7})
    );
}

#[tokio::test]
async fn patch_display_name_persists_and_trims() {
    let app = profile_app().await;
    let email = unique_email();
    register_user(&app, &email).await;
    let token = login_token(&app, &email).await;

    let res = app
        .request(patch_me(Some(&token), json!({"display_name": "  Deen  "})))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["display_name"], "Deen");

    // GET reflects the stored name.
    let res = app.request(get_me(Some(&token))).await;
    assert_eq!(res.json()["display_name"], "Deen");
}

#[tokio::test]
async fn patch_display_name_invalid_is_422() {
    let app = profile_app().await;
    let email = unique_email();
    register_user(&app, &email).await;
    let token = login_token(&app, &email).await;

    for name in ["   ", &"x".repeat(101)] {
        let res = app
            .request(patch_me(Some(&token), json!({"display_name": name})))
            .await;
        assert_eq!(
            res.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{}",
            res.text()
        );
        let json = res.json();
        assert_eq!(json["code"], "PF-2002");
        assert_eq!(json["field_errors"][0]["field"], "display_name");
    }
}

#[tokio::test]
async fn patch_email_records_pending_intent() {
    let app = profile_app().await;
    let email = unique_email();
    let user_id = register_user(&app, &email).await;
    let token = login_token(&app, &email).await;

    let res = app
        .request(patch_me(
            Some(&token),
            json!({"email": "new-address@example.com"}),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let json = res.json();
    // The intent is recorded, the address is NOT swapped.
    assert_eq!(json["pending_email"], "new-address@example.com");
    assert_eq!(json["email"], email);

    // The row agrees, and GET reflects it.
    let user = users::by_id(&app.pool, user_id).await.unwrap().unwrap();
    assert_eq!(
        user.pending_email.as_deref(),
        Some("new-address@example.com")
    );
    assert_eq!(user.email, email);
    let res = app.request(get_me(Some(&token))).await;
    assert_eq!(res.json()["pending_email"], "new-address@example.com");
}

#[tokio::test]
async fn patch_email_same_as_current_is_422() {
    let app = profile_app().await;
    let email = unique_email();
    register_user(&app, &email).await;
    let token = login_token(&app, &email).await;

    let res = app
        .request(patch_me(Some(&token), json!({"email": email})))
        .await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    let json = res.json();
    assert_eq!(json["code"], "PF-2002");
    assert_eq!(json["field_errors"][0]["field"], "email");
    assert_eq!(json["field_errors"][0]["code"], "email_unchanged");
}

#[tokio::test]
async fn patch_email_disposable_is_422() {
    let app = profile_app().await;
    let email = unique_email();
    register_user(&app, &email).await;
    let token = login_token(&app, &email).await;

    let res = app
        .request(patch_me(Some(&token), json!({"email": "x@mailinator.com"})))
        .await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    let json = res.json();
    assert_eq!(json["code"], "PF-2002");
    assert_eq!(json["field_errors"][0]["code"], "disposable_email");
}

#[tokio::test]
async fn patch_email_duplicate_is_409() {
    let app = profile_app().await;
    let email_a = unique_email();
    let email_b = unique_email();
    register_user(&app, &email_a).await;
    register_user(&app, &email_b).await;
    let token = login_token(&app, &email_a).await;

    let res = app
        .request(patch_me(Some(&token), json!({"email": email_b})))
        .await;
    assert_eq!(res.status, StatusCode::CONFLICT, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-2011");
}

#[tokio::test]
async fn patch_email_null_clears_pending_intent() {
    let app = profile_app().await;
    let email = unique_email();
    register_user(&app, &email).await;
    let token = login_token(&app, &email).await;

    let res = app
        .request(patch_me(
            Some(&token),
            json!({"display_name": "Deen", "email": "new-address@example.com"}),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["display_name"], "Deen");
    assert_eq!(res.json()["pending_email"], "new-address@example.com");

    // Both fields clear with null.
    let res = app
        .request(patch_me(
            Some(&token),
            json!({"display_name": null, "email": null}),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let json = res.json();
    assert!(json.get("display_name").is_none(), "{json}");
    assert!(json.get("pending_email").is_none(), "{json}");
}

#[tokio::test]
async fn patch_without_auth_is_401() {
    let app = profile_app().await;
    let res = app
        .request(patch_me(None, json!({"display_name": "Deen"})))
        .await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1001");
}

#[tokio::test]
async fn patch_no_fields_is_noop_200_without_audit() {
    let app = profile_app().await;
    let email = unique_email();
    let user_id = register_user(&app, &email).await;
    let token = login_token(&app, &email).await;

    let res = app.request(patch_me(Some(&token), json!({}))).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["email"], email);

    // No `profile.update` audit row: a no-op writes nothing.
    let rows = audit_logs::list_for_action(&app.pool, "profile.update")
        .await
        .expect("list audits");
    assert!(
        rows.iter()
            .all(|row| row.target_id.as_deref() != Some(&user_id.to_string())),
        "no-op PATCH must not audit: {rows:?}"
    );
}

#[tokio::test]
async fn patch_records_profile_update_audit() {
    let app = profile_app().await;
    let email = unique_email();
    let user_id = register_user(&app, &email).await;
    let token = login_token(&app, &email).await;

    let res = app
        .request(patch_me(Some(&token), json!({"display_name": "Deen"})))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());

    let rows = audit_logs::list_for_action(&app.pool, "profile.update")
        .await
        .expect("list audits");
    let row = rows
        .iter()
        .find(|row| row.target_id.as_deref() == Some(&user_id.to_string()))
        .expect("profile.update audit exists");
    assert_eq!(row.actor_user_id, Some(user_id));
    // The audit carries the changed name plainly (public profile
    // data) and never the raw email.
    assert_eq!(row.after.as_ref().unwrap()["display_name"], "Deen");
}
