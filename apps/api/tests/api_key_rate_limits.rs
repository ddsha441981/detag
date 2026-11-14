//! P1-037: per-key rate limits (FR-A-06).
//!
//! The extractor spends one token from the verified key's
//! tier bucket on every key-authenticated request (after the
//! scope check, before `note_key_use`): a default-tier key
//! (NULL tier) bursts to its configured limit and then gets
//! 429 `PF-3007` with `Retry-After` and the KEY bucket's
//! `X-RateLimit-*` trio; a `high`-tier key survives past the
//! default burst; an unknown tier name falls back to the
//! default tier; repeated trips write exactly one
//! `api_key.rate_limit_tripped` audit row per key per window;
//! buckets are per key (one key's exhaustion does not touch
//! another's); JWT callers are not key-limited at all.
//!
//! The tests run with tiny buckets (default burst 2, high
//! burst 5, refill 0, abuse threshold 2) so a handful of
//! requests proves each behaviour without waiting for a
//! refill. `GET /v1/jobs` is the probe: a scoped key reaches
//! the stub (501 = auth passed), a denied one never does.

mod common;

use axum::http::{Method, Request, StatusCode};
use common::{TestApp, TestFixtures};
use serde_json::json;
use uuid::Uuid;

/// The request id: a valid UUID, echoed by the middleware.
const REQUEST_ID: &str = "123e4567-e89b-42d3-a456-426614174107";

/// The password every test user registers with.
const PASSWORD: &str = "correct horse battery staple";

/// Test settings: tiny key buckets so a few requests prove
/// the behaviour — default burst 2 (refill 0), high burst 5
/// (refill 0), abuse threshold 2 in a 1 h window.
async fn app() -> TestApp {
    let fixtures = TestFixtures::from_env().await;
    let mut settings = fixtures.settings();
    settings.disposable_email_domains = "mailinator.com".to_string();
    settings.tos_current_version = "v1".to_string();
    settings.api_key_rate_limit_burst = 2;
    settings.api_key_rate_limit_refill_per_min = 0;
    settings.api_key_rate_limit_high_burst = 5;
    settings.api_key_rate_limit_high_refill_per_min = 0;
    settings.api_key_rate_limit_abuse_threshold = 2;
    settings.api_key_rate_limit_abuse_window_secs = 3600;
    TestApp::spawn_with_fixtures_and_settings(fixtures, settings).await
}

fn unique_email(prefix: &str) -> String {
    format!("{prefix}-{}@example.com", Uuid::now_v7())
}

/// A per-test XFF address: each request gets its own IP
/// bucket, so the tests never trip each other's IP limits
/// (the P1-003 lesson — the shared "unknown" bucket is one
/// bucket for the whole binary).
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

/// Mint an API key for the JWT caller; return (key id, secret).
async fn mint_key(app: &TestApp, token: &str, scopes: &[&str]) -> (Uuid, String) {
    let body = json!({
        "name": "test key",
        "scopes": scopes,
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
    let json = res.json();
    (
        json["id"].as_str().unwrap().parse().unwrap(),
        json["secret"].as_str().unwrap().to_string(),
    )
}

/// Register + log in + mint a key; return (user id, key id, secret).
async fn key_for(app: &TestApp, scopes: &[&str]) -> (Uuid, Uuid, String) {
    let email = unique_email("key-user");
    let user_id = register_user(app, &email).await;
    let token = login_token(app, &email).await;
    let (key_id, secret) = mint_key(app, &token, scopes).await;
    (user_id, key_id, secret)
}

/// `GET /v1/jobs` with a Bearer <redacted> key — the probe: 501
/// means auth + scope + rate limit all passed (the stub).
async fn key_jobs_list(app: &TestApp, key: &str) -> common::TestResponse {
    app.request(
        Request::builder()
            .method(Method::GET)
            .uri("/v1/jobs")
            .header("x-request-id", REQUEST_ID)
            .header("x-forwarded-for", unique_xff())
            .header("authorization", format!("Bearer {key}"))
            .body(axum::body::Body::empty())
            .unwrap(),
    )
    .await
}

/// Set a key row's `rate_limit_tier` directly (no endpoint
/// sets it yet — P1-037's tiers are operator-managed).
async fn set_tier(app: &TestApp, key_id: Uuid, tier: &str) {
    sqlx::query("UPDATE api_keys SET rate_limit_tier = $1 WHERE id = $2")
        .bind(tier)
        .bind(key_id)
        .execute(&app.pool)
        .await
        .expect("tier update");
}

/// The `api_key.rate_limit_tripped` audit rows for a key.
async fn trip_rows(app: &TestApp, key_id: Uuid) -> Vec<serde_json::Value> {
    let rows = sqlx::query(
        "SELECT \"after\" FROM audit_logs WHERE action = 'api_key.rate_limit_tripped' AND target_id = $1 ORDER BY created_at",
    )
    .bind(key_id.to_string())
    .fetch_all(&app.pool)
    .await
    .expect("audit read");
    use sqlx::Row;
    rows.iter()
        .map(|row| {
            let after: serde_json::Value = row.try_get("after").expect("after column");
            after
        })
        .collect()
}

fn header<'a>(res: &'a common::TestResponse, name: &str) -> &'a str {
    res.headers
        .get(name)
        .expect("header present")
        .to_str()
        .expect("header is ASCII")
}

#[tokio::test]
async fn default_tier_denies_after_burst_with_429_headers() {
    // P1-037: a NULL-tier key gets the default tier (burst 2
    // in these test settings): two requests reach the stub
    // (501), the third is 429 PF-3007 with Retry-After and
    // the KEY bucket's trio — Limit 2, Remaining 0.
    let app = app().await;
    let (_user_id, _key_id, secret) = key_for(&app, &["jobs:read"]).await;

    let first = key_jobs_list(&app, &secret).await;
    assert_eq!(
        first.status,
        StatusCode::NOT_IMPLEMENTED,
        "{}",
        first.text()
    );
    let second = key_jobs_list(&app, &secret).await;
    assert_eq!(
        second.status,
        StatusCode::NOT_IMPLEMENTED,
        "{}",
        second.text()
    );

    let denied = key_jobs_list(&app, &secret).await;
    assert_eq!(
        denied.status,
        StatusCode::TOO_MANY_REQUESTS,
        "{}",
        denied.text()
    );
    assert_eq!(denied.json()["code"], "PF-3007");
    assert_eq!(header(&denied, "x-ratelimit-limit"), "2");
    assert_eq!(header(&denied, "x-ratelimit-remaining"), "0");
    assert!(
        denied.headers.contains_key("x-ratelimit-reset"),
        "reset header"
    );
    let retry_after: u64 = header(&denied, "retry-after")
        .parse()
        .expect("Retry-After is seconds");
    assert!(retry_after >= 1, "Retry-After is at least 1 s");
}

#[tokio::test]
async fn high_tier_survives_past_default_burst() {
    // P1-037: a `high`-tier key (burst 5 here) answers five
    // requests — a default key would 429 on the third — and
    // its own denial names the high burst (Limit 5).
    let app = app().await;
    let (_user_id, key_id, secret) = key_for(&app, &["jobs:read"]).await;
    set_tier(&app, key_id, "high").await;

    for n in 1..=5 {
        let res = key_jobs_list(&app, &secret).await;
        assert_eq!(
            res.status,
            StatusCode::NOT_IMPLEMENTED,
            "request {n}: {}",
            res.text()
        );
    }
    let denied = key_jobs_list(&app, &secret).await;
    assert_eq!(
        denied.status,
        StatusCode::TOO_MANY_REQUESTS,
        "{}",
        denied.text()
    );
    assert_eq!(denied.json()["code"], "PF-3007");
    assert_eq!(header(&denied, "x-ratelimit-limit"), "5");
    assert_eq!(header(&denied, "x-ratelimit-remaining"), "0");
}

#[tokio::test]
async fn unknown_tier_falls_back_to_default() {
    // P1-037: a typo'd tier name degrades to the default
    // tier — it never grants a bigger bucket and never
    // denies service outright.
    let app = app().await;
    let (_user_id, key_id, secret) = key_for(&app, &["jobs:read"]).await;
    set_tier(&app, key_id, "bogus").await;

    for n in 1..=2 {
        let res = key_jobs_list(&app, &secret).await;
        assert_eq!(
            res.status,
            StatusCode::NOT_IMPLEMENTED,
            "request {n}: {}",
            res.text()
        );
    }
    let denied = key_jobs_list(&app, &secret).await;
    assert_eq!(
        denied.status,
        StatusCode::TOO_MANY_REQUESTS,
        "{}",
        denied.text()
    );
    assert_eq!(header(&denied, "x-ratelimit-limit"), "2");
}

#[tokio::test]
async fn repeated_trips_write_one_abuse_audit_row() {
    // P1-037: the abuse threshold is 2 here — six requests
    // (2 allowed, 4 denied) must write exactly ONE audit
    // row, when the count reaches the threshold; the later
    // denials in the same window write nothing.
    let app = app().await;
    let (_user_id, key_id, secret) = key_for(&app, &["jobs:read"]).await;

    for _ in 0..6 {
        key_jobs_list(&app, &secret).await;
    }
    let rows = trip_rows(&app, key_id).await;
    assert_eq!(rows.len(), 1, "one audit row per key per window: {rows:?}");
    let after = &rows[0];
    assert_eq!(after["tier"], "default");
    assert_eq!(after["trips_in_window"], 2);
    assert_eq!(after["burst"], 2);
    assert_eq!(after["window_secs"], 3600);
    assert!(
        after["key_prefix"]
            .as_str()
            .unwrap()
            .starts_with("pf_live_"),
        "prefix is the display identifier, not the secret: {after}"
    );
    assert!(
        !after.to_string().contains(&secret),
        "the audit row must never carry the secret"
    );
}

#[tokio::test]
async fn buckets_are_per_key() {
    // P1-037: exhausting one key's bucket does not touch
    // another key's — the bucket id is the key UUID, not a
    // shared prefix.
    let app = app().await;
    let (_u1, _k1, secret_a) = key_for(&app, &["jobs:read"]).await;
    let (_u2, _k2, secret_b) = key_for(&app, &["jobs:read"]).await;

    for _ in 0..3 {
        key_jobs_list(&app, &secret_a).await;
    }
    let denied = key_jobs_list(&app, &secret_a).await;
    assert_eq!(
        denied.status,
        StatusCode::TOO_MANY_REQUESTS,
        "{}",
        denied.text()
    );

    let other = key_jobs_list(&app, &secret_b).await;
    assert_eq!(
        other.status,
        StatusCode::NOT_IMPLEMENTED,
        "{}",
        other.text()
    );
}

#[tokio::test]
async fn jwt_caller_is_not_key_limited() {
    // P1-037: the key bucket applies to key callers only —
    // a JWT caller making the same five requests is bounded
    // by the IP/route buckets (120/600), not by the key tier.
    let app = app().await;
    let email = unique_email("jwt-user");
    register_user(&app, &email).await;
    let token = login_token(&app, &email).await;

    for n in 1..=5 {
        let res = app
            .request(
                Request::builder()
                    .method(Method::GET)
                    .uri("/v1/jobs")
                    .header("x-request-id", REQUEST_ID)
                    .header("x-forwarded-for", unique_xff())
                    .header("authorization", format!("Bearer {token}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(
            res.status,
            StatusCode::NOT_IMPLEMENTED,
            "request {n}: {}",
            res.text()
        );
    }
}
