//! P1-043: the admin route guard — every `/v1/admin/*` route
//! behind the matrix guard, and the audit trail verified.
//!
//! The guard landed on the 14 stub routes in this ticket (the 5
//! live routes were already guarded): `admin_stub` /
//! `admin_read_stub` in `apps/api/src/routes/admin.rs` take the
//! matrix guard, so an unauthenticated or wrong-role caller is
//! rejected before the 501, exactly like on a live route. Two
//! more routes went live since (P1-056: `GET` / `PUT
//! /v1/admin/plans`; P1-061: `POST
//! /v1/admin/billing/catalog-sync`; P1-071: `POST
//! /v1/admin/billing/subscriptions/{id}/cancel`; P1-081:
//! `GET /v1/admin/usage/{id}` + `/export`; P1-088: `GET
//! /v1/admin/audit`), so the walk
//! now covers 12 live routes and 11 stubs. The three tests:
//!
//! - `admin_guard_on_every_route` — the walk: all 23 admin
//!   routes × 5 caller kinds (anonymous, plain user, support,
//!   admin, an admin user's API key). Anonymous is 401
//!   `PF-1001`, a wrong role or a key is 403 `PF-1002`, an
//!   admitted role is 200 on the 12 live routes and 501
//!   `PF-5004` on the 11 stubs — except the catalog sync,
//!   whose admitted admin gets 503 `PF-5005` (billing is
//!   unconfigured in the test environment: no Stripe key).
//!   A route that answers 501 to an
//!   anonymous caller has lost its guard — this fails.
//! - `admin_actions_audit_actor_target_before_after` — the
//!   audit: an admin suspend through the guarded route writes
//!   exactly one `user.suspended` row carrying the admin as
//!   actor, the target id, before/after (`active` → `suspended`)
//!   and the reason (FR-W-05).
//! - `denied_and_stub_attempts_audit_nothing` — the negative:
//!   a denied suspend (plain user, 403), a suspend of an
//!   unknown id (404), and an admitted stub call (admin on
//!   quota-grant, 501) each write NO audit row for their
//!   target. The trail records actions, not attempts.
//!
//! The automatic audit middleware is P1-046's scope (route
//! metadata drives automatic audit); this ticket proves the
//! guard + the audit rows the services already write.

mod common;

use axum::http::{Method, Request, StatusCode};
use common::{TestApp, TestFixtures};
use detag_api::authz::Capability;
use detag_core::repos::audit_logs;
use serde_json::json;
use uuid::Uuid;

/// The request id: a valid UUID, echoed by the middleware.
const REQUEST_ID: &str = "123e4567-e89b-42d3-a456-426614174109";

/// A valid UUID for the `{id}` path params.
const PATH_ID: &str = "123e4567-e89b-42d3-a456-426614174110";

/// The password every test user registers with.
const PASSWORD: &str = "correct horse battery staple";

/// Test settings: the same shape as the authz matrix (generous
/// key bucket — the walk spends one key request per route, and
/// this suite asserts authorization outcomes, not rate limits).
async fn guard_app() -> TestApp {
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

/// Mint a full-scope key for the credential holder; return the
/// secret. The walk uses an admin user's key: the matrix says
/// it is 403 on every admin route ("no admin ever").
async fn create_key(app: &TestApp, token: &str) -> String {
    let body =
        json!({ "name": "guard probe", "scopes": ["jobs:read", "jobs:write", "webhooks:manage"] });
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

/// Seed an active Pro subscription for the user; return its
/// id. The cancel row in the walk needs a real subscription
/// to reach 200 (an unknown id is 404).
async fn seed_subscription(app: &TestApp, user_id: Uuid) -> Uuid {
    let plan_id: Uuid = sqlx::query_scalar("SELECT id FROM plans WHERE code = $1")
        .bind("pro")
        .fetch_one(&app.pool)
        .await
        .expect("plan id");
    sqlx::query_scalar(
        "INSERT INTO subscriptions (user_id, plan_id, provider, provider_customer_id, provider_subscription_id, status) \
         VALUES ($1, $2, 'stripe', $3, $4, 'active') RETURNING id",
    )
    .bind(user_id)
    .bind(plan_id)
    .bind(format!("cus_p1071_{}", Uuid::now_v7()))
    .bind(format!("sub_p1071_{}", Uuid::now_v7()))
    .fetch_one(&app.pool)
    .await
    .expect("seed subscription")
}

/// One guard probe: the route with an optional credential
/// (`None` = anonymous), an optional concrete `{id}`, and an
/// optional JSON body (the write rows need `{"reason": ...}`
/// so an admitted caller reaches the handler instead of a
/// body rejection).
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

/// The audit rows for one user target, oldest first.
async fn audit_for_user(app: &TestApp, user_id: Uuid) -> Vec<audit_logs::AuditLog> {
    audit_logs::list_for_target(&app.pool, "user", &user_id.to_string())
        .await
        .expect("list audit rows")
}

/// The audit rows for one user target and one action:
/// registration and login audit against the same target, so
/// an admin-action assertion must filter by action, not just
/// by target.
async fn audit_for_user_action(
    app: &TestApp,
    user_id: Uuid,
    action: &str,
) -> Vec<audit_logs::AuditLog> {
    audit_for_user(app, user_id)
        .await
        .into_iter()
        .filter(|row| row.action == action)
        .collect()
}

/// Set up the five caller kinds; return
/// (user, support, admin, admin key, admin id).
async fn callers(app: &TestApp) -> (String, String, String, String, Uuid) {
    let (_user_id, user_token) = make_role(app, &unique_email("user"), "user").await;
    let (_support_id, support_token) = make_role(app, &unique_email("support"), "support").await;
    let (admin_id, admin_token) = make_role(app, &unique_email("admin"), "admin").await;
    // P1-055: key creation is plan-gated — the admin user
    // needs a Pro grant before it can mint the probe key
    // (keys are a Pro/Business feature, FR-P-01).
    app.grant_plan(admin_id, "pro").await;
    let admin_key = create_key(app, &admin_token).await;
    (user_token, support_token, admin_token, admin_key, admin_id)
}

/// One row of the guard walk: the route, whether it is live,
/// and whether it is a read (support admitted) or a write.
struct GuardRow {
    method: &'static str,
    path: &'static str,
    live: bool,
    read: bool,
}

/// All 23 `/v1/admin/*` routes. The read flag comes from the
/// matrix: a capability whose code ends in `.read`.
const GUARD_ROWS: &[GuardRow] = &[
    GuardRow {
        method: "GET",
        path: "/v1/admin/metrics/dashboard",
        live: false,
        read: true,
    },
    GuardRow {
        method: "GET",
        path: "/v1/admin/users",
        live: true,
        read: true,
    },
    GuardRow {
        method: "POST",
        path: "/v1/admin/users/{id}/suspend",
        live: true,
        read: false,
    },
    GuardRow {
        method: "POST",
        path: "/v1/admin/users/{id}/restore",
        live: true,
        read: false,
    },
    GuardRow {
        method: "POST",
        path: "/v1/admin/users/{id}/quota-grant",
        live: false,
        read: false,
    },
    GuardRow {
        method: "POST",
        path: "/v1/admin/users/{id}/impersonate",
        live: true,
        read: false,
    },
    GuardRow {
        method: "GET",
        path: "/v1/admin/jobs",
        live: true,
        read: true,
    },
    GuardRow {
        method: "POST",
        path: "/v1/admin/jobs/{id}/reprocess",
        live: false,
        read: false,
    },
    GuardRow {
        method: "POST",
        path: "/v1/admin/jobs/{id}/cancel",
        live: false,
        read: false,
    },
    GuardRow {
        method: "GET",
        path: "/v1/admin/dlq",
        live: false,
        read: true,
    },
    GuardRow {
        method: "GET",
        path: "/v1/admin/flags",
        live: false,
        read: true,
    },
    GuardRow {
        method: "PUT",
        path: "/v1/admin/flags",
        live: false,
        read: false,
    },
    GuardRow {
        method: "GET",
        path: "/v1/admin/plans",
        live: true,
        read: true,
    },
    GuardRow {
        method: "PUT",
        path: "/v1/admin/plans",
        live: true,
        read: false,
    },
    GuardRow {
        method: "POST",
        path: "/v1/admin/billing/catalog-sync",
        live: true,
        read: false,
    },
    GuardRow {
        method: "POST",
        path: "/v1/admin/billing/subscriptions/{id}/cancel",
        live: true,
        read: false,
    },
    GuardRow {
        method: "GET",
        path: "/v1/admin/usage/{id}",
        live: true,
        read: true,
    },
    GuardRow {
        method: "GET",
        path: "/v1/admin/usage/{id}/export",
        live: true,
        read: true,
    },
    GuardRow {
        method: "GET",
        path: "/v1/admin/audit",
        live: true,
        read: true,
    },
    GuardRow {
        method: "GET",
        path: "/v1/admin/abuse-reports",
        live: false,
        read: true,
    },
    GuardRow {
        method: "POST",
        path: "/v1/admin/abuse-reports/{id}/resolve",
        live: false,
        read: false,
    },
    GuardRow {
        method: "GET",
        path: "/v1/admin/attestations",
        live: false,
        read: true,
    },
    GuardRow {
        method: "GET",
        path: "/v1/admin/system",
        live: false,
        read: true,
    },
];

#[tokio::test]
async fn admin_guard_on_every_route() {
    // The walk: all 23 admin routes × 5 caller kinds. The
    // read/write flag must agree with the matrix — a row
    // whose flag disagrees with its capability fails here,
    // so the walk cannot drift from the P1-042 table.
    for row in GUARD_ROWS {
        let method = Method::from_bytes(row.method.as_bytes()).unwrap();
        let capability = Capability::for_route(&method, row.path)
            .unwrap_or_else(|| panic!("{} {}: no matrix row", row.method, row.path));
        assert_eq!(
            capability.code().ends_with(".read"),
            row.read,
            "{} {}: read flag disagrees with the matrix",
            row.method,
            row.path
        );
    }
    assert_eq!(GUARD_ROWS.len(), 23, "one row per admin route");

    let app = guard_app().await;
    let (user_token, support_token, admin_token, admin_key, _admin_id) = callers(&app).await;
    // One sacrificial target per live write row: suspend
    // mutates it, impersonate audits against it.
    let (suspend_target, _) = make_role(&app, &unique_email("target-s"), "user").await;
    let (restore_target, _) = make_role(&app, &unique_email("target-r"), "user").await;
    let (impersonate_target, _) = make_role(&app, &unique_email("target-i"), "user").await;
    // The cancel row needs a real subscription to reach 200
    // (an unknown id is 404): seed one for a sacrificial
    // user.
    let (cancel_target_user, _) = make_role(&app, &unique_email("target-c"), "user").await;
    let cancel_target = seed_subscription(&app, cancel_target_user).await;
    // The usage rows need a real user id to reach 200 (an
    // unknown id is 404): any registered user works.
    let (usage_target, _) = make_role(&app, &unique_email("target-u"), "user").await;

    for row in GUARD_ROWS {
        // The target + body the row needs when the caller
        // is admitted past the guard.
        let (target, body) = match row.path {
            "/v1/admin/users/{id}/suspend" => (
                Some(suspend_target),
                Some(json!({ "reason": "guard probe" })),
            ),
            "/v1/admin/users/{id}/restore" => (Some(restore_target), None),
            "/v1/admin/users/{id}/impersonate" => (
                Some(impersonate_target),
                Some(json!({ "reason": "guard probe" })),
            ),
            // The cancel row needs a real subscription id +
            // a reason for the admitted caller to reach 200.
            "/v1/admin/billing/subscriptions/{id}/cancel" => (
                Some(cancel_target),
                Some(json!({ "reason": "guard probe" })),
            ),
            // The usage rows need a real user id for the
            // admitted caller to reach 200 (an unknown id
            // is 404, not 200).
            "/v1/admin/usage/{id}" | "/v1/admin/usage/{id}/export" => (Some(usage_target), None),
            // The plans PUT needs a valid body for the
            // admitted caller to reach 200: the empty edit
            // is a no-op 200 on the shared harness DB (P1-056).
            "/v1/admin/plans" if row.method == "PUT" => (None, Some(json!({ "plans": [] }))),
            _ => (None, None),
        };
        // Anonymous: 401 PF-1001 — on every route, live or
        // stub. A 501 here means the guard is missing.
        let res = probe(&app, row.method, row.path, None, target, body.clone()).await;
        assert_eq!(
            res.status,
            StatusCode::UNAUTHORIZED,
            "{} {}",
            row.method,
            row.path
        );
        assert_eq!(problem_code(&res), "PF-1001", "{} {}", row.method, row.path);
        // Plain user: 403 PF-1002 — on every route.
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
        // Support: admitted on reads, 403 on writes. Live
        // reads answer 200; stub reads answer 501 (the guard
        // passed, the handler has not landed yet).
        let res = probe(
            &app,
            row.method,
            row.path,
            Some(&support_token),
            target,
            body.clone(),
        )
        .await;
        if row.read {
            let want = if row.live {
                StatusCode::OK
            } else {
                StatusCode::NOT_IMPLEMENTED
            };
            assert_eq!(
                res.status, want,
                "{} {} (support read)",
                row.method, row.path
            );
            if !row.live {
                assert_eq!(problem_code(&res), "PF-5004", "{} {}", row.method, row.path);
            }
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
        // Admin: 200 on live routes, 501 on stubs — except
        // the catalog sync: its handler is live but billing
        // is unconfigured here (no Stripe key), so the
        // admitted admin gets 503 PF-5005 (P1-061).
        let res = probe(
            &app,
            row.method,
            row.path,
            Some(&admin_token),
            target,
            body.clone(),
        )
        .await;
        let want = if row.path == "/v1/admin/billing/catalog-sync" {
            StatusCode::SERVICE_UNAVAILABLE
        } else if row.live {
            StatusCode::OK
        } else {
            StatusCode::NOT_IMPLEMENTED
        };
        assert_eq!(
            res.status,
            want,
            "{} {} (admin): {}",
            row.method,
            row.path,
            res.text()
        );
        if row.path == "/v1/admin/billing/catalog-sync" {
            assert_eq!(problem_code(&res), "PF-5005", "{} {}", row.method, row.path);
        }
        if !row.live {
            assert_eq!(problem_code(&res), "PF-5004", "{} {}", row.method, row.path);
        }
        // An admin user's own API key: 403 PF-1002 ("no admin
        // ever") — on every route, live or stub.
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

#[tokio::test]
async fn admin_actions_audit_actor_target_before_after() {
    // The audit: an admin suspend through the guarded route
    // writes exactly one row with the actor, the target, the
    // before/after states and the reason (FR-W-05).
    let app = guard_app().await;
    let (_user_token, _support_token, admin_token, _admin_key, admin_id) = callers(&app).await;
    let (target_id, _) = make_role(&app, &unique_email("target"), "user").await;

    let res = probe(
        &app,
        "POST",
        "/v1/admin/users/{id}/suspend",
        Some(&admin_token),
        Some(target_id),
        Some(json!({ "reason": "guard audit probe" })),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());

    let rows = audit_for_user_action(&app, target_id, "user.suspended").await;
    assert_eq!(rows.len(), 1, "one audit row for the suspend");
    let row = &rows[0];
    assert_eq!(row.action, "user.suspended");
    assert_eq!(row.actor_user_id, Some(admin_id), "actor is the admin");
    assert_eq!(row.target_type, "user");
    assert_eq!(
        row.target_id.as_deref(),
        Some(target_id.to_string()).as_deref()
    );
    assert_eq!(row.reason.as_deref(), Some("guard audit probe"));
    assert_eq!(row.before.as_ref().unwrap()["status"], "active");
    assert_eq!(row.after.as_ref().unwrap()["status"], "suspended");
}

#[tokio::test]
async fn denied_and_stub_attempts_audit_nothing() {
    // The negative: denials and admitted stub calls change no
    // state, so they write no audit row. The trail records
    // actions, not attempts.
    let app = guard_app().await;
    let (user_token, _support_token, admin_token, _admin_key, _admin_id) = callers(&app).await;
    let (denied_target, _) = make_role(&app, &unique_email("denied"), "user").await;
    let (stub_target, _) = make_role(&app, &unique_email("stub"), "user").await;
    let unknown_id = Uuid::now_v7();

    // A plain user tries to suspend: 403, no audit row.
    let res = probe(
        &app,
        "POST",
        "/v1/admin/users/{id}/suspend",
        Some(&user_token),
        Some(denied_target),
        Some(json!({ "reason": "must not be audited" })),
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-1002");
    assert_eq!(
        audit_for_user_action(&app, denied_target, "user.suspended")
            .await
            .len(),
        0,
        "a denied suspend audits nothing"
    );

    // An admin suspends an unknown id: 404, no audit row.
    let res = probe(
        &app,
        "POST",
        "/v1/admin/users/{id}/suspend",
        Some(&admin_token),
        Some(unknown_id),
        Some(json!({ "reason": "must not be audited" })),
    )
    .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.text());
    assert_eq!(
        audit_for_user_action(&app, unknown_id, "user.suspended")
            .await
            .len(),
        0,
        "a 404 suspend audits nothing"
    );

    // An admin calls a stub write (quota-grant): 501, no row.
    let res = probe(
        &app,
        "POST",
        "/v1/admin/users/{id}/quota-grant",
        Some(&admin_token),
        Some(stub_target),
        Some(json!({ "reason": "must not be audited" })),
    )
    .await;
    assert_eq!(res.status, StatusCode::NOT_IMPLEMENTED, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-5004");
    assert_eq!(
        audit_for_user_action(&app, stub_target, "user.suspended")
            .await
            .len(),
        0,
        "an admitted stub call audits nothing"
    );
}
