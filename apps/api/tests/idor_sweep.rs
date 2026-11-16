//! P1-045: IDOR sweep v1 — cross-user access to an owned
//! resource returns 404, never 200 (a leak) and never 403
//! (a leak by another name: it confirms the thing exists).
//! (FR-A-07 / T13, NFR-31.)
//!
//! The sweep covers the five owned families the ticket
//! names, split by what is live today:
//!
//! - **Live — keys and sessions.** `cross_user_keys_are_not_found`:
//!   user B revokes / rotates user A's key and gets 404
//!   `PF-2010` both times, the key still authenticates
//!   afterwards (it reaches the jobs stub, 501 — a revoked
//!   key would 401), and A revoking it afterwards is 204
//!   (it existed all along; after the revoke the same key
//!   is 401). `cross_user_sessions_are_not_found`: B's own
//!   session list never contains A's session id, B deleting
//!   A's session is 404 `PF-2010`, A's session is still
//!   listed afterwards, and A deleting it is 204.
//! - **Stubs — jobs, uploads, webhook-endpoints, and the
//!   api-keys list.** `stub_resources_leak_nothing`: an
//!   authenticated cross-user caller gets 501 `PF-5004` on
//!   every one of those rows — no data, no 200, no 403.
//!   A 501 is not the 404 the spec wants; it is the honest
//!   statement for a route with no handler yet. The 404
//!   ownership proof for each family lands with its handler
//!   ticket (jobs + uploads in Phase 2, webhook-endpoints
//!   and the api-keys list with their handler tickets) —
//!   this test then gains the live rows and loses the stub
//!   rows. Until then the sweep pins that nothing leaks.
//!
//! The harness DB is shared across the tests in this binary
//! (and persists across runs), so every assertion is scoped
//! to the test's own users / key / session / job — never an
//! unfiltered count.

mod common;

use axum::http::{Method, Request, StatusCode};
use common::{TestApp, TestFixtures};
use detag_core::repos::jobs::{self, NewJob};
use serde_json::json;
use uuid::Uuid;

/// The request id: a valid UUID, echoed by the middleware.
const REQUEST_ID: &str = "123e4567-e89b-42d3-a456-426614174109";

/// A valid UUID for the `{id}` path params.
const PATH_ID: &str = "123e4567-e89b-42d3-a456-426614174110";

/// The password every test user registers with.
const PASSWORD: &str = "correct horse battery staple";

/// Test settings: the same shape as the admin guard suite
/// (generous key bucket — this suite asserts authorization
/// outcomes, not rate limits).
async fn sweep_app() -> TestApp {
    let fixtures = TestFixtures::from_env().await;
    let mut settings = fixtures.settings();
    settings.disposable_email_domains = "mailinator.com".to_string();
    settings.tos_current_version = "v1".to_string();
    settings.api_key_rate_limit_burst = 1000;
    settings.api_key_rate_limit_refill_per_min = 1000;
    TestApp::spawn_with_fixtures_and_settings(fixtures, settings).await
}

fn unique_email(prefix: &str) -> String {
    format!("{prefix}-{}@example.com", Uuid::now_v7())
}

/// A per-request XFF address: each request gets its own IP
/// bucket, so the tests never trip each other's rate limits
/// (the P1-003 lesson).
fn unique_xff() -> String {
    format!("203.0.113.{}", Uuid::now_v7())
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

/// Register + log in, return (id, token).
async fn make_user(app: &TestApp, email: &str) -> (Uuid, String) {
    let id = register_user(app, email).await;
    let token = login_token(app, email).await;
    (id, token)
}

/// Grant Pro to user A only (P1-055): key creation is
/// plan-gated, so A needs the grant to mint the probe key.
/// B stays Free — B never mints, B only probes A's key.
async fn make_key_owner(app: &TestApp, email: &str) -> (Uuid, String) {
    let (id, token) = make_user(app, email).await;
    app.grant_plan(id, "pro").await;
    (id, token)
}

/// Mint a full-scope key for the JWT holder; return (id,
/// secret). The id is what the cross-user caller must not
/// be able to touch; the secret is what proves the key is
/// still alive afterwards.
async fn create_key(app: &TestApp, token: &str) -> (Uuid, String) {
    let body =
        json!({ "name": "idor probe", "scopes": ["jobs:read", "jobs:write", "webhooks:manage"] });
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

/// Seed a job row for a user through the repo directly — job
/// creation is still the Phase 2 stub (the P1-038 pattern).
/// The row exists so the stub probes below carry a real id.
async fn seed_job(app: &TestApp, user_id: Uuid) -> Uuid {
    let idem = format!("idem-p145-{}", Uuid::now_v7());
    jobs::create(
        &app.pool,
        &NewJob {
            user_id,
            upload_id: None,
            kind: "image",
            mode: "auto",
            status: "queued",
            lane: "standard",
            params: json!({}),
            idempotency_key: Some(&idem),
            key_id: None,
        },
    )
    .await
    .expect("seed job")
    .id
}

/// One probe: a request with an optional credential (`None`
/// = anonymous), an optional concrete `{id}`, and an
/// optional JSON body.
async fn probe(
    app: &TestApp,
    method: &str,
    path: &str,
    credential: Option<&str>,
    target: Option<Uuid>,
    body: Option<serde_json::Value>,
) -> common::TestResponse {
    let uri = match target {
        Some(id) => path.replace("{id}", &id.to_string()),
        None => path.replace("{id}", PATH_ID),
    };
    let mut req = Request::builder()
        .method(Method::from_bytes(method.as_bytes()).unwrap())
        .uri(uri)
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff());
    if matches!(method, "POST" | "PUT" | "PATCH") {
        req = req.header("content-type", "application/json");
    }
    if let Some(credential) = credential {
        req = req.header("authorization", format!("Bearer {credential}"));
    }
    let body = match body {
        Some(value) => axum::body::Body::from(serde_json::to_vec(&value).unwrap()),
        None => axum::body::Body::empty(),
    };
    app.request(req.body(body).unwrap()).await
}

/// The problem `code` of a response body ("" when the body
/// is not a problem — the assertion message then shows the
/// raw body).
fn problem_code(res: &common::TestResponse) -> String {
    res.json()["code"].as_str().unwrap_or("").to_string()
}

/// The session ids in a user's own session list.
async fn own_session_ids(app: &TestApp, token: &str) -> Vec<String> {
    let res = probe(app, "GET", "/v1/me/sessions", Some(token), None, None).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    res.json()
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["id"].as_str().map(str::to_string))
        .collect()
}

#[tokio::test]
async fn cross_user_keys_are_not_found() {
    // A mints a key. B revoking it and rotating it are both
    // 404 `PF-2010` — the same answer as an unknown id, so
    // B cannot tell a foreign key from a nonexistent one.
    // The key still authenticates afterwards, and A revoking
    // it is 204 — it existed all along, and B's attempts did
    // not touch it. After the revoke the key is 401.
    let app = sweep_app().await;
    let (_a_id, a_token) = make_key_owner(&app, &unique_email("a")).await;
    let (_b_id, b_token) = make_user(&app, &unique_email("b")).await;
    let (key_id, secret) = create_key(&app, &a_token).await;

    // The key is alive: it reaches the jobs stub (501), past
    // the extractor. A dead key would 401 here.
    let res = probe(&app, "GET", "/v1/jobs", Some(&secret), None, None).await;
    assert_eq!(res.status, StatusCode::NOT_IMPLEMENTED, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-5004");

    let res = probe(
        &app,
        "DELETE",
        "/v1/api-keys/{id}",
        Some(&b_token),
        Some(key_id),
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-2010");

    let res = probe(
        &app,
        "POST",
        "/v1/api-keys/{id}/rotate",
        Some(&b_token),
        Some(key_id),
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-2010");

    // Still alive after B's attempts.
    let res = probe(&app, "GET", "/v1/jobs", Some(&secret), None, None).await;
    assert_eq!(res.status, StatusCode::NOT_IMPLEMENTED, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-5004");

    // The owner revokes it: 204. It existed all along.
    let res = probe(
        &app,
        "DELETE",
        "/v1/api-keys/{id}",
        Some(&a_token),
        Some(key_id),
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());

    // And now the same key is dead: 401, not 501.
    let res = probe(&app, "GET", "/v1/jobs", Some(&secret), None, None).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-1001");
}

#[tokio::test]
async fn cross_user_sessions_are_not_found() {
    // A logs in (one session). B's own list never contains
    // A's session id, B deleting A's session is 404
    // `PF-2010`, A's session is still listed afterwards,
    // and A deleting it is 204 — it existed all along.
    let app = sweep_app().await;
    let (_a_id, a_token) = make_user(&app, &unique_email("a")).await;
    let (_b_id, b_token) = make_user(&app, &unique_email("b")).await;

    let a_sessions = own_session_ids(&app, &a_token).await;
    assert_eq!(a_sessions.len(), 1, "one login, one session");
    let session_id: Uuid = a_sessions[0].parse().unwrap();

    let b_sessions = own_session_ids(&app, &b_token).await;
    assert!(
        !b_sessions.contains(&a_sessions[0]),
        "B's list must not contain A's session: {b_sessions:?}"
    );

    let res = probe(
        &app,
        "DELETE",
        "/v1/me/sessions/{id}",
        Some(&b_token),
        Some(session_id),
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-2010");

    let a_sessions_after = own_session_ids(&app, &a_token).await;
    assert!(
        a_sessions_after.contains(&a_sessions[0]),
        "A's session survives B's delete attempt: {a_sessions_after:?}"
    );

    let res = probe(
        &app,
        "DELETE",
        "/v1/me/sessions/{id}",
        Some(&a_token),
        Some(session_id),
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());
}

#[tokio::test]
async fn stub_resources_leak_nothing() {
    // Jobs, uploads, webhook-endpoints and the api-keys
    // list have no handler yet: an authenticated cross-user
    // caller gets 501 `PF-5004` on every row — no data, no
    // 200, no 403. The 404 ownership proof for each family
    // lands with its handler ticket; until then this pins
    // that nothing leaks.
    let app = sweep_app().await;
    let (a_id, a_token) = make_user(&app, &unique_email("a")).await;
    let (b_id, b_token) = make_user(&app, &unique_email("b")).await;
    // P1-055: `POST /v1/webhook-endpoints` is plan-gated —
    // B needs a Pro grant to reach the 501 stub on that row
    // (a Free caller is 403 `PF-3003` there, proven in
    // feature_flags.rs); the other rows are unaffected.
    app.grant_plan(b_id, "pro").await;
    let job_id = seed_job(&app, a_id).await;

    // (method, path, target) — "job" = the seeded job id,
    // "any" = a random id (nothing exists behind it).
    let rows: &[(&str, &str, &str)] = &[
        ("GET", "/v1/jobs", "none"),
        ("POST", "/v1/jobs", "none"),
        ("GET", "/v1/jobs/{id}", "job"),
        ("POST", "/v1/jobs/{id}/cancel", "job"),
        ("POST", "/v1/jobs/{id}/retry", "job"),
        ("GET", "/v1/jobs/{id}/events", "job"),
        ("GET", "/v1/jobs/{id}/result", "job"),
        ("GET", "/v1/jobs/{id}/report", "job"),
        ("POST", "/v1/jobs/{id}/attestation", "job"),
        ("POST", "/v1/uploads", "none"),
        ("GET", "/v1/uploads/{id}", "any"),
        ("POST", "/v1/uploads/{id}/complete", "any"),
        ("POST", "/v1/uploads/{id}/abort", "any"),
        ("GET", "/v1/api-keys", "none"),
        ("GET", "/v1/webhook-endpoints", "none"),
        ("POST", "/v1/webhook-endpoints", "none"),
        ("DELETE", "/v1/webhook-endpoints/{id}", "any"),
        ("GET", "/v1/webhook-endpoints/{id}/deliveries", "any"),
    ];
    assert_eq!(rows.len(), 18, "one row per stub resource route");
    for (method, path, target) in rows {
        let target = match *target {
            "job" => Some(job_id),
            "any" => Some(Uuid::now_v7()),
            _ => None,
        };
        let body = if matches!(*method, "POST" | "PUT" | "PATCH") {
            Some(json!({}))
        } else {
            None
        };
        let res = probe(&app, method, path, Some(&b_token), target, body).await;
        assert_eq!(
            res.status,
            StatusCode::NOT_IMPLEMENTED,
            "{method} {path} (cross-user): {}",
            res.text()
        );
        assert_eq!(problem_code(&res), "PF-5004", "{method} {path}");
    }

    // The plain upload stubs take no credential at all — an
    // anonymous caller gets the same 501, never data.
    let res = probe(
        &app,
        "GET",
        "/v1/uploads/{id}",
        None,
        Some(Uuid::now_v7()),
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::NOT_IMPLEMENTED, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-5004");

    // The owner sees the same 501 on their own job — the
    // stub answers identically to everyone, so the 501
    // itself leaks nothing about ownership.
    let res = probe(
        &app,
        "GET",
        "/v1/jobs/{id}",
        Some(&a_token),
        Some(job_id),
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::NOT_IMPLEMENTED, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-5004");
}
