//! P1-040: the API-key ACL matrix — one row per `/v1` route.
//!
//! The matrix is the registry the extractor's scope table
//! answers to: for every route in `ROUTE_TABLE`, the row
//! states what an API key gets, and the test walks every row
//! live against a spawned app. The four outcomes:
//!
//! - `Allowed(scope)` — a key carrying `scope` reaches the
//!   handler (501 `PF-5004` on the stubs — auth passed, the
//!   handler has not landed; 200 on the one live handler,
//!   `GET /v1/usage`, P1-080); no credential is 401
//!   `PF-1001`. Today only the jobs nest (9 routes), the
//!   webhook-endpoints nest (4), and the usage read (1) are
//!   gated this way.
//! - `Forbidden` — any key is 403 `PF-1002` (the route is not
//!   in the extractor's scope table — fail closed); no
//!   credential is 401. This covers the live authed routes
//!   keys must never call (profile, sessions, key management,
//!   `logout-all`, all 27 admin routes since P1-043 put the
//!   matrix guard on the stubs too).
//! - `Unprotected` — NO credential is checked at all: the
//!   plain 501 stub answers anonymous and key alike. These
//!   are gaps, recorded honestly, not 403s: the route's
//!   handler ticket adds the extractor/guard and flips the
//!   row then. One of them is public BY DESIGN, not a gap —
//!   `GET /v1/plans` (the plan matrix is public data) — the
//!   row's note says so. The provider webhooks and the abuse
//!   intake are `Ignored`, not `Unprotected`: their handlers
//!   take no credential either, but they are live — a key
//!   changes nothing, so the anonymous and keyed responses
//!   are identical (the `Ignored` proof, below).
//! - `Ignored` — the handler takes no credential (the auth
//!   routes, the provider webhooks, the abuse intake): the key
//!   key responses are identical. The proof is the equality
//!   itself — had an extractor run, the key would be 403
//!   while anonymous is 401, so equal statuses mean no
//!   extractor ran.
//!
//! The registry rule (the pattern P1-042's full roles matrix
//! reuses): adding a route to `ROUTE_TABLE` without a matrix
//! row fails `matrix_covers_every_v1_route`; a row naming a
//! route that does not exist fails it too. When a stub lands
//! its handler, its row flips (usually `Unprotected` ->
//! `Allowed` or `Forbidden`) in the same commit.
//!
//! "No admin ever, no billing" (the ticket's phrasing):
//! every `/v1/admin/*` row is `Forbidden` — live or guarded
//! stub since P1-043 (never `Allowed`), and every billing row
//! is `Unprotected` except the webhook-endpoints nest, which
//! is key-managed by design, the checkout endpoint, which
//! is `Forbidden` since P1-062 (JWT only), the portal
//! endpoint, which is `Forbidden` since P1-067 (JWT only),
//! the invoices endpoint, which is `Forbidden` since P1-068
//! (JWT only), the usage endpoint, which is `Allowed` with
//! `jobs:read` since P1-080 (a read of the key owner's own
//! metering — the billing exception, ADR-0044), and the
//! provider webhooks, which are `Ignored`
//! since P1-063 (Stripe) and P1-073 (Razorpay) — live
//! handlers, signature-verified, no Bearer credential — a
//! key changes nothing. An admin user's
//! OWN key is 403 on the live admin routes too
//! (`admin_users_key_cannot_reach_admin_routes`) — the guard
//! rejects a key-carrying context outright, so a future
//! mis-rowed scope table cannot smuggle an admin key in.

mod common;

use std::collections::HashSet;

use axum::http::{Method, Request, StatusCode};
use common::{TestApp, TestFixtures};
use detag_api::routes::ROUTE_TABLE;
use serde_json::json;
use uuid::Uuid;

/// The request id: a valid UUID, echoed by the middleware.
const REQUEST_ID: &str = "123e4567-e89b-42d3-a456-426614174109";

/// A valid UUID for the `{id}` path params (the extractor
/// rejects before any handler reads it; the stubs never read
/// it either).
const PATH_ID: &str = "123e4567-e89b-42d3-a456-426614174110";

/// The password every test user registers with.
const PASSWORD: &str = "correct horse battery staple";

/// What an API key gets on one route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyOutcome {
    /// A key carrying this scope reaches the handler.
    Allowed(&'static str),
    /// Any key is 403 `PF-1002`.
    Forbidden,
    /// No credential is checked at all (501 for everyone).
    Unprotected,
    /// The handler takes no credential (anonymous == key).
    Ignored,
}

/// One matrix row: the route, the key's outcome, and why.
struct MatrixRow {
    method: &'static str,
    path: &'static str,
    outcome: KeyOutcome,
    note: &'static str,
}

/// The matrix: all 77 `/v1` routes (auth 24, uploads 4,
/// jobs 9, billing 12, admin 28 — the 27 `/v1/admin/*`
/// routes plus the public `POST /v1/abuse` intake). The meta
/// routes (`/healthz`, `/readyz`) are unversioned and public,
/// so they have no row — the completeness test asserts
/// exactly the 75 `/v1` entries.
const MATRIX: &[MatrixRow] = &[
    // auth (24)
    MatrixRow { method: "POST", path: "/v1/auth/register", outcome: KeyOutcome::Ignored, note: "no credential — the handler validates the body" },
    MatrixRow { method: "POST", path: "/v1/auth/verify-email", outcome: KeyOutcome::Ignored, note: "no credential — the handler validates the body" },
    MatrixRow { method: "POST", path: "/v1/auth/resend-verification", outcome: KeyOutcome::Ignored, note: "no credential — the handler validates the body" },
    MatrixRow { method: "POST", path: "/v1/auth/confirm-email-change", outcome: KeyOutcome::Ignored, note: "no credential — the handler validates the body" },
    MatrixRow { method: "POST", path: "/v1/auth/login", outcome: KeyOutcome::Ignored, note: "no credential — the handler validates the body" },
    MatrixRow { method: "POST", path: "/v1/auth/refresh", outcome: KeyOutcome::Ignored, note: "cookie credential, not Bearer — a key is ignored" },
    MatrixRow { method: "POST", path: "/v1/auth/logout", outcome: KeyOutcome::Ignored, note: "cookie credential, not Bearer — a key is ignored" },
    MatrixRow { method: "POST", path: "/v1/auth/logout-all", outcome: KeyOutcome::Forbidden, note: "takes AuthContext; keys are not in the scope table" },
    MatrixRow { method: "POST", path: "/v1/auth/forgot-password", outcome: KeyOutcome::Ignored, note: "no credential — the handler validates the body" },
    MatrixRow { method: "POST", path: "/v1/auth/reset-password", outcome: KeyOutcome::Ignored, note: "no credential — the handler validates the body" },
    MatrixRow { method: "GET", path: "/v1/me", outcome: KeyOutcome::Forbidden, note: "takes AuthContext; keys are not in the scope table" },
    MatrixRow { method: "PATCH", path: "/v1/me", outcome: KeyOutcome::Forbidden, note: "takes AuthContext; keys are not in the scope table" },
    MatrixRow { method: "DELETE", path: "/v1/me", outcome: KeyOutcome::Forbidden, note: "takes AuthContext; keys are not in the scope table" },
    MatrixRow { method: "POST", path: "/v1/me/consent", outcome: KeyOutcome::Forbidden, note: "takes AuthContext; keys are not in the scope table" },
    MatrixRow { method: "GET", path: "/v1/me/onboarding", outcome: KeyOutcome::Forbidden, note: "live handler (P1-096) behind AuthContext; keys are not in the scope table" },
    MatrixRow { method: "GET", path: "/v1/me/email-preferences", outcome: KeyOutcome::Forbidden, note: "live handler (P1-103) behind AuthContext; keys are not in the scope table" },
    MatrixRow { method: "PUT", path: "/v1/me/email-preferences", outcome: KeyOutcome::Forbidden, note: "live handler (P1-103) behind AuthContext; keys are not in the scope table" },
    MatrixRow { method: "GET", path: "/v1/me/sessions", outcome: KeyOutcome::Forbidden, note: "takes AuthContext; keys are not in the scope table" },
    MatrixRow { method: "DELETE", path: "/v1/me/sessions/{id}", outcome: KeyOutcome::Forbidden, note: "takes AuthContext; keys are not in the scope table" },
    MatrixRow { method: "POST", path: "/v1/me/export", outcome: KeyOutcome::Unprotected, note: "gap — plain stub, no credential checked; the handler ticket adds the extractor" },
    MatrixRow { method: "GET", path: "/v1/api-keys", outcome: KeyOutcome::Unprotected, note: "gap — plain stub (the list handler has not landed), no credential checked" },
    MatrixRow { method: "POST", path: "/v1/api-keys", outcome: KeyOutcome::Forbidden, note: "live handler; a key cannot mint a key (P1-039)" },
    MatrixRow { method: "DELETE", path: "/v1/api-keys/{id}", outcome: KeyOutcome::Forbidden, note: "live handler; a key cannot revoke (P1-036)" },
    MatrixRow { method: "POST", path: "/v1/api-keys/{id}/rotate", outcome: KeyOutcome::Forbidden, note: "live handler; a key cannot rotate (P1-035)" },
    // uploads (4)
    MatrixRow { method: "POST", path: "/v1/uploads", outcome: KeyOutcome::Unprotected, note: "gap — plain stub, no credential checked; the handler ticket adds the extractor" },
    MatrixRow { method: "POST", path: "/v1/uploads/{id}/complete", outcome: KeyOutcome::Unprotected, note: "gap — plain stub, no credential checked; the handler ticket adds the extractor" },
    MatrixRow { method: "POST", path: "/v1/uploads/{id}/abort", outcome: KeyOutcome::Unprotected, note: "gap — plain stub, no credential checked; the handler ticket adds the extractor" },
    MatrixRow { method: "GET", path: "/v1/uploads/{id}", outcome: KeyOutcome::Unprotected, note: "gap — plain stub, no credential checked; the handler ticket adds the extractor" },
    // jobs (9)
    MatrixRow { method: "POST", path: "/v1/jobs", outcome: KeyOutcome::Allowed("jobs:write"), note: "stub_authed; jobs writes need jobs:write" },
    MatrixRow { method: "GET", path: "/v1/jobs", outcome: KeyOutcome::Allowed("jobs:read"), note: "stub_authed; jobs reads need jobs:read" },
    MatrixRow { method: "GET", path: "/v1/jobs/{id}", outcome: KeyOutcome::Allowed("jobs:read"), note: "stub_authed; jobs reads need jobs:read" },
    MatrixRow { method: "POST", path: "/v1/jobs/{id}/cancel", outcome: KeyOutcome::Allowed("jobs:write"), note: "stub_authed; jobs writes need jobs:write" },
    MatrixRow { method: "POST", path: "/v1/jobs/{id}/retry", outcome: KeyOutcome::Allowed("jobs:write"), note: "stub_authed; jobs writes need jobs:write" },
    MatrixRow { method: "GET", path: "/v1/jobs/{id}/events", outcome: KeyOutcome::Allowed("jobs:read"), note: "stub_authed; jobs reads need jobs:read" },
    MatrixRow { method: "GET", path: "/v1/jobs/{id}/result", outcome: KeyOutcome::Allowed("jobs:read"), note: "stub_authed; jobs reads need jobs:read" },
    MatrixRow { method: "GET", path: "/v1/jobs/{id}/report", outcome: KeyOutcome::Allowed("jobs:read"), note: "stub_authed; jobs reads need jobs:read" },
    MatrixRow { method: "POST", path: "/v1/jobs/{id}/attestation", outcome: KeyOutcome::Allowed("jobs:write"), note: "stub_authed; jobs writes need jobs:write" },
    // billing (12)
    MatrixRow { method: "GET", path: "/v1/plans", outcome: KeyOutcome::Unprotected, note: "public by design — the plan matrix is public data, no credential" },
    MatrixRow { method: "GET", path: "/v1/billing/subscription", outcome: KeyOutcome::Unprotected, note: "gap — plain stub, no credential checked; the handler ticket adds the extractor (no billing for keys)" },
    MatrixRow { method: "POST", path: "/v1/billing/checkout", outcome: KeyOutcome::Forbidden, note: "JWT only — the route is not in the extractor's scope table, so any key is 403 (P1-062)" },
    MatrixRow { method: "POST", path: "/v1/billing/portal", outcome: KeyOutcome::Forbidden, note: "JWT only — the route is not in the extractor's scope table, so any key is 403 (P1-067)" },
    MatrixRow { method: "GET", path: "/v1/billing/invoices", outcome: KeyOutcome::Forbidden, note: "JWT only — the route is not in the extractor's scope table, so any key is 403 (P1-068)" },
    MatrixRow { method: "GET", path: "/v1/usage", outcome: KeyOutcome::Allowed("jobs:read"), note: "live handler (P1-080) — usage is a read of the key owner's own metering, the billing exception to no-billing-for-keys (ADR-0044); a key with jobs:read gets 200, not the stub's 501" },
    MatrixRow { method: "POST", path: "/v1/webhooks/stripe", outcome: KeyOutcome::Ignored, note: "public by design — provider webhook, signature-verified, no Bearer credential; a key changes nothing (both probes 503 here: billing unconfigured in the matrix app, P1-063)" },
    MatrixRow { method: "POST", path: "/v1/webhooks/razorpay", outcome: KeyOutcome::Ignored, note: "public by design — provider webhook, signature-verified, no Bearer credential; a key changes nothing (both probes 404 here: the flag is off in the matrix app, P1-073)" },
    MatrixRow { method: "GET", path: "/v1/webhook-endpoints", outcome: KeyOutcome::Allowed("webhooks:manage"), note: "stub_authed; the user's own webhook endpoints are key-managed" },
    MatrixRow { method: "POST", path: "/v1/webhook-endpoints", outcome: KeyOutcome::Allowed("webhooks:manage"), note: "stub_authed; the user's own webhook endpoints are key-managed" },
    MatrixRow { method: "DELETE", path: "/v1/webhook-endpoints/{id}", outcome: KeyOutcome::Allowed("webhooks:manage"), note: "stub_authed; the user's own webhook endpoints are key-managed" },
    MatrixRow { method: "GET", path: "/v1/webhook-endpoints/{id}/deliveries", outcome: KeyOutcome::Allowed("webhooks:manage"), note: "stub_authed; the user's own webhook endpoints are key-managed" },
    // admin (28) — no admin ever for keys
    MatrixRow { method: "GET", path: "/v1/admin/metrics/dashboard", outcome: KeyOutcome::Forbidden, note: "guarded stub (P1-043): the matrix guard rejects a key outright (no admin ever)" },
    MatrixRow { method: "GET", path: "/v1/admin/users", outcome: KeyOutcome::Forbidden, note: "live handler behind AdminReadContext; the extractor rejects keys first" },
    MatrixRow { method: "GET", path: "/v1/admin/users/{id}", outcome: KeyOutcome::Forbidden, note: "live handler (P1-106) behind AdminReadContext; the extractor rejects keys first" },
    MatrixRow { method: "POST", path: "/v1/admin/users/{id}/suspend", outcome: KeyOutcome::Forbidden, note: "live handler behind AdminContext; the guard rejects a key outright" },
    MatrixRow { method: "POST", path: "/v1/admin/users/{id}/restore", outcome: KeyOutcome::Forbidden, note: "live handler behind AdminContext; the guard rejects a key outright" },
    MatrixRow { method: "POST", path: "/v1/admin/users/{id}/warn", outcome: KeyOutcome::Forbidden, note: "live handler behind AdminContext; the guard rejects a key outright" },
    MatrixRow { method: "POST", path: "/v1/admin/users/{id}/terminate", outcome: KeyOutcome::Forbidden, note: "live handler behind AdminContext; the guard rejects a key outright" },
    MatrixRow { method: "POST", path: "/v1/admin/users/{id}/quota-grant", outcome: KeyOutcome::Forbidden, note: "live handler (P1-107) behind AdminContext; the guard rejects a key outright" },
    MatrixRow { method: "POST", path: "/v1/admin/users/{id}/impersonate", outcome: KeyOutcome::Forbidden, note: "live handler behind AdminContext; the guard rejects a key outright" },
    MatrixRow { method: "GET", path: "/v1/admin/jobs", outcome: KeyOutcome::Forbidden, note: "live handler behind AdminReadContext; the extractor rejects keys first" },
    MatrixRow { method: "POST", path: "/v1/admin/jobs/{id}/reprocess", outcome: KeyOutcome::Forbidden, note: "guarded stub (P1-043): the matrix guard rejects a key outright (no admin ever)" },
    MatrixRow { method: "POST", path: "/v1/admin/jobs/{id}/cancel", outcome: KeyOutcome::Forbidden, note: "guarded stub (P1-043): the matrix guard rejects a key outright (no admin ever)" },
    MatrixRow { method: "GET", path: "/v1/admin/dlq", outcome: KeyOutcome::Forbidden, note: "guarded stub (P1-043): the matrix guard rejects a key outright (no admin ever)" },
    MatrixRow { method: "GET", path: "/v1/admin/flags", outcome: KeyOutcome::Forbidden, note: "guarded stub (P1-043): the matrix guard rejects a key outright (no admin ever)" },
    MatrixRow { method: "PUT", path: "/v1/admin/flags", outcome: KeyOutcome::Forbidden, note: "guarded stub (P1-043): the matrix guard rejects a key outright (no admin ever)" },
    MatrixRow { method: "GET", path: "/v1/admin/plans", outcome: KeyOutcome::Forbidden, note: "live handler (P1-056) behind AdminReadContext; the extractor rejects keys first" },
    MatrixRow { method: "PUT", path: "/v1/admin/plans", outcome: KeyOutcome::Forbidden, note: "live handler (P1-056) behind AdminContext; the guard rejects a key outright (no admin ever)" },
    MatrixRow { method: "POST", path: "/v1/admin/billing/catalog-sync", outcome: KeyOutcome::Forbidden, note: "live handler (P1-061) behind AdminContext; the guard rejects a key outright (no admin ever)" },
    MatrixRow { method: "POST", path: "/v1/admin/billing/subscriptions/{id}/cancel", outcome: KeyOutcome::Forbidden, note: "live handler (P1-071) behind AdminContext; the guard rejects a key outright (no admin ever)" },
    MatrixRow { method: "POST", path: "/v1/admin/billing/subscriptions/{id}/refund", outcome: KeyOutcome::Forbidden, note: "live handler (P1-108) behind AdminContext; the guard rejects a key outright (no admin ever)" },
    MatrixRow { method: "GET", path: "/v1/admin/usage/{id}", outcome: KeyOutcome::Forbidden, note: "live handler (P1-081) behind AdminReadContext; the extractor rejects keys first" },
    MatrixRow { method: "GET", path: "/v1/admin/usage/{id}/export", outcome: KeyOutcome::Forbidden, note: "live handler (P1-081) behind AdminReadContext; the extractor rejects keys first" },
    MatrixRow { method: "GET", path: "/v1/admin/audit", outcome: KeyOutcome::Forbidden, note: "live handler (P1-088) behind AdminReadContext; the extractor rejects keys first" },
    MatrixRow { method: "GET", path: "/v1/admin/abuse-reports", outcome: KeyOutcome::Forbidden, note: "guarded stub (P1-043): the matrix guard rejects a key outright (no admin ever)" },
    MatrixRow { method: "POST", path: "/v1/admin/abuse-reports/{id}/resolve", outcome: KeyOutcome::Forbidden, note: "guarded stub (P1-043): the matrix guard rejects a key outright (no admin ever)" },
    MatrixRow { method: "GET", path: "/v1/admin/attestations", outcome: KeyOutcome::Forbidden, note: "live handler (P1-098) behind AdminReadContext; the extractor rejects keys first" },
    MatrixRow { method: "GET", path: "/v1/admin/system", outcome: KeyOutcome::Forbidden, note: "guarded stub (P1-043): the matrix guard rejects a key outright (no admin ever)" },
    MatrixRow { method: "POST", path: "/v1/abuse", outcome: KeyOutcome::Ignored, note: "public by design — abuse intake takes no credential; the handler validates the body (P1-089)" },
];

/// The live admin routes (the ones with real handlers +
/// guards today) — the admin-key test walks exactly these.
const LIVE_ADMIN_ROUTES: &[(&str, &str)] = &[
    ("GET", "/v1/admin/users"),
    ("GET", "/v1/admin/users/{id}"),
    ("POST", "/v1/admin/users/{id}/suspend"),
    ("POST", "/v1/admin/users/{id}/restore"),
    ("POST", "/v1/admin/users/{id}/impersonate"),
    ("GET", "/v1/admin/jobs"),
    ("POST", "/v1/admin/billing/catalog-sync"),
    ("POST", "/v1/admin/billing/subscriptions/{id}/cancel"),
    ("GET", "/v1/admin/usage/{id}"),
    ("GET", "/v1/admin/usage/{id}/export"),
];

/// Test settings: a generous key bucket (burst 1000, refill
/// 1000/min) so the ~13 allowed spends of the matrix walk
/// never trip the P1-037 rate limit — this suite asserts
/// authorization outcomes, not rate limits (those live in
/// `api_key_rate_limits.rs`).
async fn matrix_app() -> TestApp {
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

/// Fill `{id}` params with a concrete UUID (the extractor
/// rejects before any handler reads it; the stubs never read
/// it either).
fn concrete_path(path: &str) -> String {
    path.replace("{id}", PATH_ID)
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

/// Register a user, promote them to admin in the database,
/// log in, return (id, token). The promotion is a direct
/// UPDATE — there is no self-serve way to become an admin
/// (the role is set by the owner out of band).
async fn make_admin(app: &TestApp, email: &str) -> (Uuid, String) {
    let id = register_user(app, email).await;
    sqlx::query("UPDATE users SET role = 'admin' WHERE id = $1")
        .bind(id)
        .execute(&app.pool)
        .await
        .expect("promote to admin");
    // P1-055: key creation is plan-gated — the admin needs a
    // Pro grant to mint a key (keys are a Pro feature).
    app.grant_plan(id, "pro").await;
    let token = login_token(app, email).await;
    (id, token)
}

/// Mint a key for the JWT holder; return (id, full secret).
async fn create_key(app: &TestApp, token: &str, name: &str, scopes: &[&str]) -> (Uuid, String) {
    let body = json!({ "name": name, "scopes": scopes });
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
    let created = res.json();
    (
        created["id"].as_str().unwrap().parse().unwrap(),
        created["secret"].as_str().unwrap().to_string(),
    )
}

/// One matrix probe: the row's route, with an optional
/// Bearer credential (`None` = anonymous). Empty body;
/// POST/PUT/PATCH carry a JSON content type so the live
/// auth handlers reach their body validation (422) instead
/// of a content-type rejection.
async fn probe(
    app: &TestApp,
    method: &str,
    path: &str,
    credential: Option<&str>,
) -> common::TestResponse {
    let mut req = Request::builder()
        .method(Method::from_bytes(method.as_bytes()).unwrap())
        .uri(concrete_path(path))
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff());
    if matches!(method, "POST" | "PUT" | "PATCH") {
        req = req.header("content-type", "application/json");
    }
    if let Some(credential) = credential {
        req = req.header("authorization", format!("Bearer {credential}"));
    }
    app.request(req.body(axum::body::Body::empty()).unwrap())
        .await
}

/// The problem `code` of a response body ("" when the body
/// is not a problem — the assertion message then shows the
/// raw body).
fn problem_code(res: &common::TestResponse) -> String {
    res.json()["code"].as_str().unwrap_or("").to_string()
}

#[test]
fn matrix_covers_every_v1_route() {
    // The registry rule: every /v1 route in ROUTE_TABLE has
    // exactly one matrix row, and every row names a real
    // route. Add a route without a row (or a row without a
    // route) and this test fails — the matrix cannot drift
    // from the router silently.
    let v1_routes: Vec<(&str, &str)> = ROUTE_TABLE
        .iter()
        .filter(|(_, path)| path.starts_with("/v1"))
        .map(|(method, path)| (*method, *path))
        .collect();
    assert_eq!(
        v1_routes.len(),
        77,
        "ROUTE_TABLE must hold the 77 /v1 routes (auth 24 + uploads 4 + jobs 9 + billing 12 + admin 28)"
    );
    assert_eq!(
        MATRIX.len(),
        77,
        "the matrix must hold one row per /v1 route ({} rows today)",
        MATRIX.len()
    );
    let table_set: HashSet<(&str, &str)> = v1_routes.iter().copied().collect();
    let mut matrix_set = HashSet::new();
    for row in MATRIX {
        assert!(
            table_set.contains(&(row.method, row.path)),
            "matrix row names a route that is not in ROUTE_TABLE: {} {}",
            row.method,
            row.path
        );
        assert!(
            matrix_set.insert((row.method, row.path)),
            "duplicate matrix row: {} {}",
            row.method,
            row.path
        );
    }
    for (method, path) in &v1_routes {
        assert!(
            matrix_set.contains(&(*method, *path)),
            "ROUTE_TABLE route has no matrix row: {method} {path}"
        );
    }
}

#[tokio::test]
async fn api_key_matrix() {
    // Walk every row with a full-scope key (all three
    // scopes) plus the anonymous control, and assert the
    // row's outcome — including the problem code, so a 403
    // from the wrong layer (or a 501 that is really a 401)
    // cannot hide behind the status.
    let app = matrix_app().await;
    let email = unique_email("matrix");
    let user_id = register_user(&app, &email).await;
    let token = login_token(&app, &email).await;
    // P1-055: key creation is plan-gated — the matrix user
    // needs a Pro grant to mint the probe key.
    app.grant_plan(user_id, "pro").await;
    let (_key_id, secret) = create_key(
        &app,
        &token,
        "matrix",
        &["jobs:write", "jobs:read", "webhooks:manage"],
    )
    .await;

    for row in MATRIX {
        let anon = probe(&app, row.method, row.path, None).await;
        let keyed = probe(&app, row.method, row.path, Some(&secret)).await;
        let label = format!("{} {} ({})", row.method, row.path, row.note);
        match row.outcome {
            KeyOutcome::Allowed(_scope) => {
                assert_eq!(anon.status, StatusCode::UNAUTHORIZED, "{label}: anonymous");
                assert_eq!(problem_code(&anon), "PF-1001", "{label}: anonymous code");
                if row.method == "GET" && row.path == "/v1/usage" {
                    // P1-080: the usage handler is live — a
                    // scoped key gets 200 with the usage shape
                    // (today + 30-day buckets + the per-key
                    // breakdown), not the stub's 501.
                    assert_eq!(
                        keyed.status,
                        StatusCode::OK,
                        "{label}: key with the scope reaches the live handler — {}",
                        keyed.text()
                    );
                    let body = keyed.json();
                    assert!(body["today"].is_object(), "{label}: today: {body}");
                    assert_eq!(
                        body["days"].as_array().map(|days| days.len()),
                        Some(30),
                        "{label}: 30 buckets: {body}"
                    );
                    assert!(body["by_key"].is_array(), "{label}: by_key: {body}");
                } else {
                    assert_eq!(
                        keyed.status,
                        StatusCode::NOT_IMPLEMENTED,
                        "{label}: key with the scope reaches the stub — {}",
                        keyed.text()
                    );
                    assert_eq!(problem_code(&keyed), "PF-5004", "{label}: keyed code");
                }
            }
            KeyOutcome::Forbidden => {
                assert_eq!(anon.status, StatusCode::UNAUTHORIZED, "{label}: anonymous");
                assert_eq!(problem_code(&anon), "PF-1001", "{label}: anonymous code");
                assert_eq!(
                    keyed.status,
                    StatusCode::FORBIDDEN,
                    "{label}: any key is 403 — {}",
                    keyed.text()
                );
                assert_eq!(problem_code(&keyed), "PF-1002", "{label}: keyed code");
            }
            KeyOutcome::Unprotected => {
                assert_eq!(
                    anon.status,
                    StatusCode::NOT_IMPLEMENTED,
                    "{label}: anonymous reaches the plain stub — {}",
                    anon.text()
                );
                assert_eq!(problem_code(&anon), "PF-5004", "{label}: anonymous code");
                assert_eq!(
                    keyed.status,
                    StatusCode::NOT_IMPLEMENTED,
                    "{label}: the key is not checked either — {}",
                    keyed.text()
                );
                assert_eq!(problem_code(&keyed), "PF-5004", "{label}: keyed code");
            }
            KeyOutcome::Ignored => {
                assert_eq!(
                    keyed.status, anon.status,
                    "{label}: the key must not change the response (anonymous {}, key {})",
                    anon.status, keyed.status
                );
                assert_ne!(
                    keyed.status,
                    StatusCode::FORBIDDEN,
                    "{label}: a key is never 403 on a route that takes no credential"
                );
            }
        }
    }
}

#[tokio::test]
async fn scope_mismatch_is_forbidden_on_every_gated_route() {
    // Every `Allowed` row, with a key that lacks the row's
    // scope: 403 `PF-1002`. Denied requests spend no rate
    // limit token (P1-037), so one key per scope class
    // covers all its rows.
    let app = matrix_app().await;
    let email = unique_email("matrix-mismatch");
    let user_id = register_user(&app, &email).await;
    let token = login_token(&app, &email).await;
    // P1-055: key creation is plan-gated — the mismatch user
    // needs a Pro grant to mint the probe keys.
    app.grant_plan(user_id, "pro").await;
    let (_read_id, read_secret) = create_key(&app, &token, "reader", &["jobs:read"]).await;
    let (_write_id, write_secret) = create_key(&app, &token, "writer", &["jobs:write"]).await;

    for row in MATRIX {
        let KeyOutcome::Allowed(scope) = row.outcome else {
            continue;
        };
        // A key carrying any scope BUT the required one.
        let wrong = if scope == "jobs:read" {
            &write_secret
        } else {
            &read_secret
        };
        let res = probe(&app, row.method, row.path, Some(wrong)).await;
        assert_eq!(
            res.status,
            StatusCode::FORBIDDEN,
            "{} {} with a key lacking {scope} is 403 — {}",
            row.method,
            row.path,
            res.text()
        );
        assert_eq!(
            problem_code(&res),
            "PF-1002",
            "{} {} with a key lacking {scope}: code",
            row.method,
            row.path
        );
    }
}

#[tokio::test]
async fn admin_users_key_cannot_reach_admin_routes() {
    // "No admin ever": a key minted by an ADMIN user is 403
    // on every live admin route — the extractor rejects keys
    // on admin routes, and the admin guard rejects a
    // key-carrying context outright, so even an admin-owned
    // key cannot admin through it.
    let app = matrix_app().await;
    let (_admin_id, admin_token) = make_admin(&app, &unique_email("matrix-admin")).await;
    let (_key_id, admin_secret) = create_key(
        &app,
        &admin_token,
        "admin key",
        &["jobs:write", "jobs:read", "webhooks:manage"],
    )
    .await;

    for (method, path) in LIVE_ADMIN_ROUTES {
        let res = probe(&app, method, path, Some(&admin_secret)).await;
        assert_eq!(
            res.status,
            StatusCode::FORBIDDEN,
            "{method} {path} with an admin user's key is 403 — {}",
            res.text()
        );
        assert_eq!(
            problem_code(&res),
            "PF-1002",
            "{method} {path} with an admin user's key: code"
        );
    }
}
