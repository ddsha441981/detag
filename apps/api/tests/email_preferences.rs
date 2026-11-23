//! P1-103: email preferences, end to end.
//!
//! Through the real router (`TestApp` oneshot) against the
//! migrated scratch database: a fresh user sees both prefs
//! on; a PUT flips one pref, preserves the other and every
//! other settings key, and writes exactly one
//! `user.email_prefs_updated` audit row; a no-op PUT writes
//! nothing; an empty body, an unknown field, and a non-bool
//! value are all 422; an API key is 403 on both verbs and no
//! credential is 401. The prefs' effect on sending is proven
//! where sending happens: the weekly-digest sender skips an
//! opted-out user (P1-101's live tests) and the drain parks a
//! marketing row for an opted-out user (the mailer drain
//! tests) — this file proves the endpoint itself.

mod common;

use axum::http::{Method, Request, StatusCode};
use common::{TestApp, TestFixtures};
use detag_core::repos::{audit_logs, users};
use serde_json::{json, Value};
use uuid::Uuid;

/// The request id: a valid UUID, echoed by the middleware.
const REQUEST_ID: &str = "123e4567-e89b-42d3-a456-426614174105";

/// The password every test user registers with.
const PASSWORD: &str = "correct horse battery staple";

/// App with both current versions at v1 (the default).
async fn prefs_app() -> TestApp {
    let fixtures = TestFixtures::from_env().await;
    let mut settings = fixtures.settings();
    settings.disposable_email_domains = String::new();
    settings.tos_current_version = "v1".to_string();
    settings.aup_current_version = "v1".to_string();
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

/// Mint an API key over HTTP (Pro-granted: key creation is
/// plan-gated since P1-055). Returns the secret.
async fn create_key(app: &TestApp, token: &str) -> String {
    let body = json!({
        "name": "prefs probe",
        "scopes": ["jobs:read"],
    });
    let res = app
        .request(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/api-keys")
                .header("x-request-id", REQUEST_ID)
                .header("x-forwarded-for", unique_xff())
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(axum::body::Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.text());
    res.json()["secret"].as_str().unwrap().to_string()
}

fn get_prefs(token: Option<&str>) -> Request<axum::body::Body> {
    let mut req = Request::builder()
        .method(Method::GET)
        .uri("/v1/me/email-preferences")
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff());
    if let Some(token) = token {
        req = req.header("authorization", format!("Bearer {token}"));
    }
    req.body(axum::body::Body::empty()).unwrap()
}

fn put_prefs(token: Option<&str>, body: Value) -> Request<axum::body::Body> {
    let mut req = Request::builder()
        .method(Method::PUT)
        .uri("/v1/me/email-preferences")
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff())
        .header("content-type", "application/json");
    if let Some(token) = token {
        req = req.header("authorization", format!("Bearer {token}"));
    }
    req.body(axum::body::Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

/// The `user.email_prefs_updated` audit rows for one user.
async fn prefs_audit_rows(app: &TestApp, user_id: Uuid) -> Vec<Value> {
    audit_logs::list_for_action(&app.pool, "user.email_prefs_updated")
        .await
        .expect("audit read")
        .into_iter()
        .filter(|row| row.target_id.as_deref() == Some(&user_id.to_string()))
        .map(|row| {
            json!({
                "actor": row.actor_user_id,
                "before": row.before,
                "after": row.after,
            })
        })
        .collect()
}

#[tokio::test]
async fn get_returns_defaults_for_a_new_user() {
    let app = prefs_app().await;
    let email = unique_email("prefs-default");
    register_user(&app, &email).await;
    let token = login_token(&app, &email).await;

    let res = app.request(get_prefs(Some(&token))).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(
        res.json(),
        json!({"weekly_digest": true, "marketing": true}),
        "a fresh user is opted in to both"
    );
}

#[tokio::test]
async fn put_updates_one_pref_and_preserves_the_other_and_settings() {
    let app = prefs_app().await;
    let email = unique_email("prefs-put");
    let user_id = register_user(&app, &email).await;
    let token = login_token(&app, &email).await;

    // A settings key this ticket does not own: the merge must
    // keep it — a prefs write never replaces the whole object.
    let user = users::by_id(&app.pool, user_id)
        .await
        .expect("read user")
        .expect("user exists");
    let mut settings = user.settings.clone();
    settings["feature_x"] = json!("keep-me");
    users::update_settings(&app.pool, user_id, &settings)
        .await
        .expect("seed an unrelated settings key");

    let res = app
        .request(put_prefs(Some(&token), json!({"marketing": false})))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(
        res.json(),
        json!({"weekly_digest": true, "marketing": false}),
        "only the named pref flips"
    );

    // The GET agrees, and the unrelated key survived the write.
    let res = app.request(get_prefs(Some(&token))).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(
        res.json(),
        json!({"weekly_digest": true, "marketing": false})
    );
    let user = users::by_id(&app.pool, user_id)
        .await
        .expect("read user")
        .expect("user exists");
    assert_eq!(user.settings["feature_x"], json!("keep-me"));
    assert_eq!(user.settings["marketing"], json!(false));

    // Both at once, the other direction: the second PUT flips
    // marketing back on and the digest off.
    let res = app
        .request(put_prefs(
            Some(&token),
            json!({"weekly_digest": false, "marketing": true}),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(
        res.json(),
        json!({"weekly_digest": false, "marketing": true})
    );
}

#[tokio::test]
async fn put_is_audited_and_noop_writes_nothing() {
    let app = prefs_app().await;
    let email = unique_email("prefs-audit");
    let user_id = register_user(&app, &email).await;
    let token = login_token(&app, &email).await;

    let before = prefs_audit_rows(&app, user_id).await;
    assert!(before.is_empty(), "no prefs audit rows at register");

    let res = app
        .request(put_prefs(Some(&token), json!({"weekly_digest": false})))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());

    let after = prefs_audit_rows(&app, user_id).await;
    assert_eq!(after.len(), 1, "exactly one audit row for the change");
    assert_eq!(after[0]["actor"], json!(user_id.to_string()));
    assert_eq!(
        after[0]["before"],
        json!({"weekly_digest": true, "marketing": true})
    );
    assert_eq!(
        after[0]["after"],
        json!({"weekly_digest": false, "marketing": true})
    );

    // A no-op PUT (the same values again) is a 200 with no
    // second row — a request that changes nothing writes
    // nothing (the PATCH /v1/me + consent precedent).
    let res = app
        .request(put_prefs(Some(&token), json!({"weekly_digest": false})))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(
        prefs_audit_rows(&app, user_id).await.len(),
        1,
        "a no-op PUT writes no second audit row"
    );
}

#[tokio::test]
async fn put_rejects_empty_body_unknown_field_and_non_bool() {
    let app = prefs_app().await;
    let email = unique_email("prefs-422");
    register_user(&app, &email).await;
    let token = login_token(&app, &email).await;

    // An empty body names no pref — 422.
    let res = app.request(put_prefs(Some(&token), json!({}))).await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );

    // An unknown field is 422, not silently dropped — a
    // typo'd key must not look like a successful opt-out.
    let res = app
        .request(put_prefs(
            Some(&token),
            json!({"weekly_digest": false, "weekley_digest": true}),
        ))
        .await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );

    // A non-bool value is 422 (serde rejects the body before
    // validation runs — the P0-101 malformed-body contract).
    let res = app
        .request(put_prefs(Some(&token), json!({"marketing": "no"})))
        .await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );

    // None of the three changed anything: the prefs are still
    // the defaults.
    let res = app.request(get_prefs(Some(&token))).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(
        res.json(),
        json!({"weekly_digest": true, "marketing": true})
    );
}

#[tokio::test]
async fn api_key_is_403_and_anon_is_401() {
    let app = prefs_app().await;
    let email = unique_email("prefs-key");
    let user_id = register_user(&app, &email).await;
    let token = login_token(&app, &email).await;
    app.grant_plan(user_id, "pro").await;
    let key = create_key(&app, &token).await;

    // An API key is 403 on both verbs (the route is not in
    // the extractor's scope table — the P1-095 consent
    // precedent).
    let res = app.request(get_prefs(Some(&key))).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    let res = app
        .request(put_prefs(Some(&key), json!({"marketing": false})))
        .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());

    // No credential is 401 on both verbs.
    let res = app.request(get_prefs(None)).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    let res = app
        .request(put_prefs(None, json!({"marketing": false})))
        .await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());

    // The rejected calls changed nothing.
    let res = app.request(get_prefs(Some(&token))).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(
        res.json(),
        json!({"weekly_digest": true, "marketing": true})
    );
}
