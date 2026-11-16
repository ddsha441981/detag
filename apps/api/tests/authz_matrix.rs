//! P1-042: the roles + permission matrix — one row per admin route.
//!
//! The matrix is the registry the guards in `crate::auth`
//! answer to: for every `/v1/admin/*` route, the row states
//! its capability (the code constant in `crate::authz`) and
//! whether the route is live. The three tests:
//!
//! - `matrix_covers_every_admin_route` — the registry rule:
//!   every admin route in `ROUTE_TABLE` has exactly one
//!   matrix row, and every row names a real admin route.
//!   Add an admin route without a row and this fails.
//! - `capability_truth_table_matches_the_matrix` — the unit
//!   registry: each row's capability resolves through
//!   `Capability::for_route`, and the role table behind it
//!   admits exactly admin + support on read rows and admin
//!   only on write/impersonate rows.
//! - `admin_matrix_live` — the live walk: the 7 live routes
//!   × 5 caller kinds (anonymous, plain user, support,
//!   admin, an admin user's API key), asserting the status
//!   the matrix predicts — 401 `PF-1001` with no credential,
//!   403 `PF-1002` for a wrong role or a key ("no admin
//!   ever"), 200 for an admitted role.
//!
//! The 12 stub rows are enforced promises since P1-043
//! (`admin_stub` / `admin_read_stub` — guard first, 501
//! after): the live walk in `admin_guard.rs` asserts the
//! guard (401 / 403 / 501) on every stub, and the handler
//! ticket replaces the stub with the real handler keeping
//! the same guard type. Their rows are pinned here so the
//! intended capability is recorded before the guard lands.

mod common;

use std::collections::HashSet;

use axum::http::{Method, Request, StatusCode};
use common::{TestApp, TestFixtures};
use detag_api::auth::Role;
use detag_api::authz::{role_has_capability, Capability};
use detag_api::routes::ROUTE_TABLE;
use serde_json::json;
use uuid::Uuid;

/// The request id: a valid UUID, echoed by the middleware.
const REQUEST_ID: &str = "123e4567-e89b-42d3-a456-426614174109";

/// A valid UUID for the `{id}` path params (the extractor
/// rejects before any handler reads it; the live handlers
/// read it only after the guard passes — the walk passes a
/// real target id for those).
const PATH_ID: &str = "123e4567-e89b-42d3-a456-426614174110";

/// The password every test user registers with.
const PASSWORD: &str = "correct horse battery staple";

/// One matrix row: the route, its capability, and whether
/// the route is live (guarded) or a stub (guard lands with
/// its handler ticket, P1-043).
struct MatrixRow {
    method: &'static str,
    path: &'static str,
    capability: Capability,
    live: bool,
}

/// The matrix: all 19 `/v1/admin/*` routes. The public
/// `POST /v1/abuse` intake lives in the admin module but is
/// not an admin route — it has no row (and no capability).
const MATRIX: &[MatrixRow] = &[
    // Live (5)
    MatrixRow {
        method: "GET",
        path: "/v1/admin/users",
        capability: Capability::AdminUsersRead,
        live: true,
    },
    MatrixRow {
        method: "GET",
        path: "/v1/admin/jobs",
        capability: Capability::AdminJobsRead,
        live: true,
    },
    MatrixRow {
        method: "POST",
        path: "/v1/admin/users/{id}/suspend",
        capability: Capability::AdminUsersWrite,
        live: true,
    },
    MatrixRow {
        method: "POST",
        path: "/v1/admin/users/{id}/restore",
        capability: Capability::AdminUsersWrite,
        live: true,
    },
    MatrixRow {
        method: "POST",
        path: "/v1/admin/users/{id}/impersonate",
        capability: Capability::AdminUsersImpersonate,
        live: true,
    },
    // Stubs (14) — guard lands with the handler ticket (P1-043)
    MatrixRow {
        method: "GET",
        path: "/v1/admin/metrics/dashboard",
        capability: Capability::AdminDashboardRead,
        live: false,
    },
    MatrixRow {
        method: "POST",
        path: "/v1/admin/users/{id}/quota-grant",
        capability: Capability::AdminUsersWrite,
        live: false,
    },
    MatrixRow {
        method: "POST",
        path: "/v1/admin/jobs/{id}/reprocess",
        capability: Capability::AdminJobsWrite,
        live: false,
    },
    MatrixRow {
        method: "POST",
        path: "/v1/admin/jobs/{id}/cancel",
        capability: Capability::AdminJobsWrite,
        live: false,
    },
    MatrixRow {
        method: "GET",
        path: "/v1/admin/dlq",
        capability: Capability::AdminDlqRead,
        live: false,
    },
    MatrixRow {
        method: "GET",
        path: "/v1/admin/flags",
        capability: Capability::AdminFlagsRead,
        live: false,
    },
    MatrixRow {
        method: "PUT",
        path: "/v1/admin/flags",
        capability: Capability::AdminFlagsWrite,
        live: false,
    },
    MatrixRow {
        method: "GET",
        path: "/v1/admin/plans",
        capability: Capability::AdminPlansRead,
        live: true,
    },
    MatrixRow {
        method: "PUT",
        path: "/v1/admin/plans",
        capability: Capability::AdminPlansWrite,
        live: true,
    },
    MatrixRow {
        method: "GET",
        path: "/v1/admin/audit",
        capability: Capability::AdminAuditRead,
        live: false,
    },
    MatrixRow {
        method: "GET",
        path: "/v1/admin/abuse-reports",
        capability: Capability::AdminAbuseRead,
        live: false,
    },
    MatrixRow {
        method: "POST",
        path: "/v1/admin/abuse-reports/{id}/resolve",
        capability: Capability::AdminAbuseWrite,
        live: false,
    },
    MatrixRow {
        method: "GET",
        path: "/v1/admin/attestations",
        capability: Capability::AdminAttestationsRead,
        live: false,
    },
    MatrixRow {
        method: "GET",
        path: "/v1/admin/system",
        capability: Capability::AdminSystemRead,
        live: false,
    },
];

#[test]
fn matrix_covers_every_admin_route() {
    // The registry rule: every /v1/admin/* route in
    // ROUTE_TABLE has exactly one matrix row, and every row
    // names a real admin route. Add an admin route without a
    // row (or a row without a route) and this test fails —
    // the matrix cannot drift from the router silently.
    let admin_routes: Vec<(&str, &str)> = ROUTE_TABLE
        .iter()
        .filter(|(_, path)| path.starts_with("/v1/admin"))
        .map(|(method, path)| (*method, *path))
        .collect();
    assert_eq!(
        admin_routes.len(),
        19,
        "ROUTE_TABLE must hold the 19 /v1/admin/* routes (the public POST /v1/abuse intake is not one)"
    );
    assert_eq!(
        MATRIX.len(),
        19,
        "the matrix must hold one row per admin route ({} rows today)",
        MATRIX.len()
    );
    let table_set: HashSet<(&str, &str)> = admin_routes.iter().copied().collect();
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
    for route in &admin_routes {
        assert!(
            matrix_set.contains(route),
            "ROUTE_TABLE admin route has no matrix row: {} {}",
            route.0,
            route.1
        );
    }
}

#[test]
fn capability_truth_table_matches_the_matrix() {
    // Each row's capability resolves through for_route,
    // and the role table behind it admits exactly the roles
    // the ticket promises: reads admin + support, writes
    // and impersonation admin only, nobody else anything.
    for row in MATRIX {
        let method = Method::from_bytes(row.method.as_bytes()).unwrap();
        assert_eq!(
            Capability::for_route(&method, row.path),
            Some(row.capability),
            "{} {}: for_route must return the row's capability",
            row.method,
            row.path
        );
        let is_read = row.capability.code().ends_with(".read");
        for (role, want) in [
            (Role::Admin, true),
            (Role::Support, is_read),
            (Role::User, false),
            (Role::Anonymous, false),
        ] {
            assert_eq!(
                role_has_capability(role, row.capability),
                want,
                "{} {} ({}): role {role:?}",
                row.method,
                row.path,
                row.capability.code()
            );
        }
    }
    // The public abuse intake is not an admin route: no row.
    assert_eq!(
        Capability::for_route(&Method::POST, "/v1/abuse"),
        None,
        "POST /v1/abuse must not map to a capability"
    );
}

/// Test settings: the same shape as the API-key matrix
/// (generous key bucket — the walk spends one key request
/// per live route, and this suite asserts authorization
/// outcomes, not rate limits).
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

/// Register a user, set its role in the database, log in,
/// return (id, token). The role is set by a direct UPDATE —
/// there is no self-serve way to become an admin or support
/// (the role is set by the owner out of band).
async fn make_role(app: &TestApp, email: &str, role: &str) -> (Uuid, String) {
    let id = register_user(app, email).await;
    sqlx::query("UPDATE users SET role = $1 WHERE id = $2")
        .bind(role)
        .bind(id)
        .execute(&app.pool)
        .await
        .expect("set role");
    let token = login_token(app, email).await;
    (id, token)
}

/// Mint a full-scope key for the JWT holder; return the
/// secret. The walk uses an admin user's key: the matrix
/// says it is 403 on every admin route ("no admin ever").
async fn create_key(app: &TestApp, token: &str) -> String {
    let body =
        json!({ "name": "matrix probe", "scopes": ["jobs:read", "jobs:write", "webhooks:manage"] });
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

/// One matrix probe: the row's route with an optional
/// Bearer credential (`None` = anonymous), an optional
/// concrete `{id}`, and an optional JSON body (the write
/// rows need `{"reason": ...}` so an admitted caller
/// reaches the handler instead of a body rejection).
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

#[tokio::test]
async fn admin_matrix_live() {
    // The live walk: the 7 live rows × 5 caller kinds.
    // Order per row is anonymous → user → support → admin
    // → key, so the mutating admin call (suspend,
    // impersonate) runs last and cannot affect the earlier
    // probes — the guard rejects those before any handler
    // reads the target anyway.
    let app = matrix_app().await;
    let (_user_id, user_token) = make_role(&app, &unique_email("user"), "user").await;
    let (_support_id, support_token) = make_role(&app, &unique_email("support"), "support").await;
    let (admin_id, admin_token) = make_role(&app, &unique_email("admin"), "admin").await;
    // P1-055: key creation is plan-gated — the admin user
    // needs a Pro grant to mint the probe key.
    app.grant_plan(admin_id, "pro").await;
    let admin_key = create_key(&app, &admin_token).await;
    // One sacrificial target per write row: suspend mutates
    // it, impersonate audits against it.
    let (suspend_target, _) = make_role(&app, &unique_email("target-s"), "user").await;
    let (restore_target, _) = make_role(&app, &unique_email("target-r"), "user").await;
    let (impersonate_target, _) = make_role(&app, &unique_email("target-i"), "user").await;

    for row in MATRIX.iter().filter(|row| row.live) {
        let is_read = row.capability.code().ends_with(".read");
        // The target + body the row needs when the caller
        // is admitted past the guard.
        let (target, body) = match row.path {
            "/v1/admin/users/{id}/suspend" => (
                Some(suspend_target),
                Some(json!({ "reason": "authz matrix probe" })),
            ),
            "/v1/admin/users/{id}/restore" => (Some(restore_target), None),
            "/v1/admin/users/{id}/impersonate" => (
                Some(impersonate_target),
                Some(json!({ "reason": "authz matrix probe" })),
            ),
            // The plans PUT needs a valid body for the
            // admitted caller to reach 200: the empty edit
            // is a no-op 200 on the shared harness DB (P1-056).
            "/v1/admin/plans" if row.method == "PUT" => (None, Some(json!({ "plans": [] }))),
            _ => (None, None),
        };
        // Anonymous: 401 PF-1001.
        let res = probe(&app, row.method, row.path, None, target, body.clone()).await;
        assert_eq!(
            res.status,
            StatusCode::UNAUTHORIZED,
            "{} {}",
            row.method,
            row.path
        );
        assert_eq!(problem_code(&res), "PF-1001", "{} {}", row.method, row.path);
        // Plain user: 403 PF-1002.
        let res = probe(
            &app,
            row.method,
            row.path,
            Some(&user_token),
            target,
            body.clone(),
        )
        .await;
        assert_eq!(
            res.status,
            StatusCode::FORBIDDEN,
            "{} {}",
            row.method,
            row.path
        );
        assert_eq!(problem_code(&res), "PF-1002", "{} {}", row.method, row.path);
        // Support: 200 on reads, 403 PF-1002 on writes.
        let res = probe(
            &app,
            row.method,
            row.path,
            Some(&support_token),
            target,
            body.clone(),
        )
        .await;
        if is_read {
            assert_eq!(
                res.status,
                StatusCode::OK,
                "{} {} (support read)",
                row.method,
                row.path
            );
        } else {
            assert_eq!(
                res.status,
                StatusCode::FORBIDDEN,
                "{} {} (support write)",
                row.method,
                row.path
            );
            assert_eq!(problem_code(&res), "PF-1002", "{} {}", row.method, row.path);
        }
        // Admin: 200.
        let res = probe(
            &app,
            row.method,
            row.path,
            Some(&admin_token),
            target,
            body.clone(),
        )
        .await;
        assert_eq!(
            res.status,
            StatusCode::OK,
            "{} {} (admin): {}",
            row.method,
            row.path,
            res.text()
        );
        // An admin user's own API key: 403 PF-1002 ("no admin ever").
        let res = probe(&app, row.method, row.path, Some(&admin_key), target, body).await;
        assert_eq!(
            res.status,
            StatusCode::FORBIDDEN,
            "{} {} (admin key)",
            row.method,
            row.path
        );
        assert_eq!(problem_code(&res), "PF-1002", "{} {}", row.method, row.path);
    }
}
