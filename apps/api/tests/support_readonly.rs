//! P1-044: support is read-only — the role can read every
//! admin view and write nothing (FR-A-07).
//!
//! The matrix already says so (P1-042) and the guard walk
//! already probes it as one caller kind among five (P1-043),
//! but this ticket pins the support contract on its own,
//! from the support role's point of view, and proves the two
//! things a status-only walk cannot:
//!
//! - `support_reads_live_views` — support reads the four
//!   live views and sees real rows: the users search returns
//!   the seeded users (email, role, status), the user view
//!   returns the owner's composite (profile, Free plan, the
//!   seeded job, the login session — no secrets), the jobs
//!   list returns the seeded job (kind, status, no key
//!   material — a web-created job shows no prefix), and the
//!   plans list returns the three seeded plans (P1-056).
//! - `support_writes_forbidden_and_change_nothing` — every
//!   write route answers 403 `PF-1002` to support, and the
//!   denial changes nothing: a suspend attempt leaves the
//!   target active, a restore attempt leaves a suspended
//!   target suspended, an impersonate attempt mints no token
//!   and writes no audit row (the stub writes are 403 before
//!   their 501, so they never run either).
//! - `support_reads_stub_views` — support is admitted on
//!   the 4 stub read routes too (501 `PF-5004`: the guard
//!   passed, the handler has not landed yet).
//! - `support_matrix_is_read_only` — the matrix assert:
//!   support is admitted exactly on the `.read`
//!   capabilities, admin on all of them, plain user and
//!   anonymous on none; every admin route in the route
//!   table maps to exactly one capability.
//!
//! The harness DB is shared across the tests in this binary
//! (and persists across runs), so every list assertion
//! filters by the test's own prefix / `user_id` — an
//! unfiltered list can never assert an exact count.

mod common;

use axum::http::{Method, Request, StatusCode};
use common::{TestApp, TestFixtures};
use detag_api::auth::Role;
use detag_api::authz::{role_has_capability, Capability};
use detag_api::routes::ROUTE_TABLE;
use detag_core::repos::audit_logs;
use detag_core::repos::jobs::{self, NewJob};
use detag_core::repos::users;
use serde_json::json;
use std::collections::HashSet;
use uuid::Uuid;

/// The request id: a valid UUID, echoed by the middleware.
const REQUEST_ID: &str = "123e4567-e89b-42d3-a456-426614174109";

/// The password every test user registers with.
const PASSWORD: &str = "correct horse battery staple";

/// Test settings: the same shape as the admin guard suite
/// (generous key bucket — this suite asserts authorization
/// outcomes, not rate limits).
async fn support_app() -> TestApp {
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

/// Register a user, set its role in the database, log in,
/// return (id, token). The role is set by a direct UPDATE —
/// there is no self-serve way to become support (the role is
/// set by the owner out of band).
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

/// Seed a job row for a user through the repo directly — job
/// creation is still the Phase 2 stub (the P1-038 pattern).
async fn seed_job(app: &TestApp, user_id: Uuid) -> Uuid {
    let idem = format!("idem-p144-{}", Uuid::now_v7());
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

/// One support probe: a request with the support token, an
/// optional concrete `{id}`, and an optional JSON body.
async fn probe(
    app: &TestApp,
    method: &str,
    path: &str,
    token: &str,
    target: Option<Uuid>,
    body: Option<serde_json::Value>,
) -> common::TestResponse {
    let uri = match target {
        Some(id) => path.replace("{id}", &id.to_string()),
        None => path.replace("{id}", "123e4567-e89b-42d3-a456-426614174110"),
    };
    let mut req = Request::builder()
        .method(Method::from_bytes(method.as_bytes()).unwrap())
        .uri(uri)
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff());
    if matches!(method, "POST" | "PUT" | "PATCH") {
        req = req.header("content-type", "application/json");
    }
    req = req.header("authorization", format!("Bearer {token}"));
    let body = match body {
        Some(value) => axum::body::Body::from(serde_json::to_vec(&value).unwrap()),
        None => axum::body::Body::empty(),
    };
    app.request(req.body(body).unwrap()).await
}

/// A GET with the support token (the read views take a query
/// string, so they go through this helper, not `probe`).
async fn get_as(app: &TestApp, uri: &str, token: &str) -> common::TestResponse {
    app.request(
        Request::builder()
            .method(Method::GET)
            .uri(uri)
            .header("x-request-id", REQUEST_ID)
            .header("x-forwarded-for", unique_xff())
            .header("authorization", format!("Bearer {token}"))
            .body(axum::body::Body::empty())
            .unwrap(),
    )
    .await
}

/// The problem `code` of a response body ("" when the body
/// is not a problem — the assertion message then shows the
/// raw body).
fn problem_code(res: &common::TestResponse) -> String {
    res.json()["code"].as_str().unwrap_or("").to_string()
}

/// A user's current status from the database.
async fn user_status(app: &TestApp, user_id: Uuid) -> String {
    users::by_id(&app.pool, user_id)
        .await
        .expect("read user")
        .expect("user exists")
        .status
}

/// The audit rows for one user target and one action.
async fn audit_for_user_action(
    app: &TestApp,
    user_id: Uuid,
    action: &str,
) -> Vec<audit_logs::AuditLog> {
    audit_logs::list_for_target(&app.pool, "user", &user_id.to_string())
        .await
        .expect("list audit rows")
        .into_iter()
        .filter(|row| row.action == action)
        .collect()
}

/// One write row: the route, the target it needs when the
/// caller is admitted, and the body the handler expects.
struct WriteRow {
    method: &'static str,
    path: &'static str,
    /// Which sacrificial target this row needs: "suspend"
    /// (an active user), "restore" (a suspended user),
    /// "impersonate" (any user), "job" (a seeded job), or
    /// none (no `{id}` in the path).
    target: &'static str,
}

/// All 11 write rows the test walks: 4 user writes +
/// quota-grant + impersonation + 2 job writes + flags +
/// plans + resolve. (The catalog-sync and billing-cancel
/// writes are not walked here — the guard walk covers
/// them; this test's rows are the ones with a sacrificial
/// target to assert unchanged afterwards.)
const WRITE_ROWS: &[WriteRow] = &[
    WriteRow {
        method: "POST",
        path: "/v1/admin/users/{id}/suspend",
        target: "suspend",
    },
    WriteRow {
        method: "POST",
        path: "/v1/admin/users/{id}/restore",
        target: "restore",
    },
    WriteRow {
        method: "POST",
        path: "/v1/admin/users/{id}/warn",
        target: "suspend",
    },
    WriteRow {
        method: "POST",
        path: "/v1/admin/users/{id}/terminate",
        target: "restore",
    },
    WriteRow {
        method: "POST",
        path: "/v1/admin/users/{id}/quota-grant",
        target: "impersonate",
    },
    WriteRow {
        method: "POST",
        path: "/v1/admin/users/{id}/impersonate",
        target: "impersonate",
    },
    WriteRow {
        method: "POST",
        path: "/v1/admin/jobs/{id}/reprocess",
        target: "job",
    },
    WriteRow {
        method: "POST",
        path: "/v1/admin/jobs/{id}/cancel",
        target: "job",
    },
    WriteRow {
        method: "PUT",
        path: "/v1/admin/flags",
        target: "none",
    },
    WriteRow {
        method: "PUT",
        path: "/v1/admin/plans",
        target: "none",
    },
    WriteRow {
        method: "POST",
        path: "/v1/admin/abuse-reports/{id}/resolve",
        target: "job",
    },
];

/// The 4 stub read routes (the live reads are covered by
/// `support_reads_live_views`, which checks users/jobs/plans).
const STUB_READ_ROWS: &[(&str, &str)] = &[
    ("GET", "/v1/admin/metrics/dashboard"),
    ("GET", "/v1/admin/dlq"),
    ("GET", "/v1/admin/flags"),
    ("GET", "/v1/admin/system"),
];

#[test]
fn support_matrix_is_read_only() {
    // The matrix assert: support is admitted exactly on
    // the `.read` capabilities — every read, no write, no
    // impersonation. Admin is admitted on all of them; a
    // plain user and an anonymous caller on none.
    for cap in Capability::all() {
        let is_read = cap.code().ends_with(".read");
        assert_eq!(
            role_has_capability(Role::Support, *cap),
            is_read,
            "{}: support must be admitted iff the capability is a read",
            cap.code()
        );
        assert!(
            role_has_capability(Role::Admin, *cap),
            "{}: admin",
            cap.code()
        );
        assert!(
            !role_has_capability(Role::User, *cap),
            "{}: plain user",
            cap.code()
        );
        assert!(
            !role_has_capability(Role::Anonymous, *cap),
            "{}: anonymous",
            cap.code()
        );
    }
    // Every admin route in the route table maps to exactly
    // one capability — a route without a row fails closed
    // in the guard, so the table and the matrix cannot drift
    // apart without this failing.
    let admin_routes: Vec<(&str, &str)> = ROUTE_TABLE
        .iter()
        .filter(|(_, path)| path.starts_with("/v1/admin"))
        .map(|(method, path)| (*method, *path))
        .collect();
    assert_eq!(admin_routes.len(), 26, "26 admin routes");
    let mut seen = HashSet::new();
    for (method, path) in &admin_routes {
        let m = Method::from_bytes(method.as_bytes()).unwrap();
        assert!(
            Capability::for_route(&m, path).is_some(),
            "{method} {path}: no matrix row"
        );
        assert!(seen.insert((method, path)), "duplicate route row");
    }
}

#[tokio::test]
async fn support_reads_live_views() {
    // Support reads the four live views and sees real rows
    // — not just a 200: the users search returns the seeded
    // users with their role and status, the user view returns
    // the owner's composite, the jobs list returns the seeded
    // job with its kind and status and no key material (a
    // web-created job shows no prefix), and the plans list
    // returns the three seeded plans (P1-056).
    let app = support_app().await;
    let (_support_id, support_token) = make_role(&app, &unique_email("support"), "support").await;
    let prefix = Uuid::now_v7().to_string();
    let email_a = format!("{prefix}-a@example.com");
    let email_b = format!("{prefix}-b@example.com");
    let (owner_id, _) = make_role(&app, &email_a, "user").await;
    let (_, _) = make_role(&app, &email_b, "user").await;
    let job_id = seed_job(&app, owner_id).await;

    let res = get_as(
        &app,
        &format!("/v1/admin/users?query={prefix}"),
        &support_token,
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let items = res.json()["items"].as_array().unwrap().clone();
    let emails: HashSet<String> = items
        .iter()
        .filter_map(|item| item["email"].as_str().map(str::to_string))
        .collect();
    assert!(
        emails.contains(&email_a) && emails.contains(&email_b),
        "both seeded users are listed: {emails:?}"
    );
    for item in &items {
        assert_eq!(item["role"], "user", "{item}");
        assert_eq!(item["status"], "active", "{item}");
    }

    let res = get_as(
        &app,
        &format!("/v1/admin/jobs?user_id={owner_id}"),
        &support_token,
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert!(
        !res.text().contains("secret"),
        "the jobs view must never carry key material: {}",
        res.text()
    );
    let items = res.json()["items"].as_array().unwrap().clone();
    assert_eq!(items.len(), 1, "one job for this owner: {items:?}");
    let job = &items[0];
    assert_eq!(job["id"], job_id.to_string(), "{job}");
    assert_eq!(job["user_id"], owner_id.to_string(), "{job}");
    assert_eq!(job["kind"], "image", "{job}");
    assert_eq!(job["status"], "queued", "{job}");
    assert!(job["key_prefix"].is_null(), "{job}");

    // The plans view (P1-056): the three seeded plans,
    // cheapest first — support reads the catalog, never
    // edits it (the write is 403, proven above).
    let res = get_as(&app, "/v1/admin/plans", &support_token).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let plans = res.json()["plans"].as_array().unwrap().clone();
    assert_eq!(plans.len(), 3, "three seeded plans: {plans:?}");
    let codes: Vec<&str> = plans
        .iter()
        .map(|plan| plan["code"].as_str().unwrap())
        .collect();
    assert_eq!(codes, vec!["free", "pro", "business"]);
    assert_eq!(plans[1]["amount"], 49900);

    // The user view (P1-106): the owner's composite — the
    // profile (email, role, status), the Free plan (no
    // subscription), the seeded job, and the login session —
    // with no secret anywhere in the body.
    let res = get_as(&app, &format!("/v1/admin/users/{owner_id}"), &support_token).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert!(
        !res.text().contains("password_hash") && !res.text().contains("refresh_hash"),
        "the user view must never carry secrets: {}",
        res.text()
    );
    let view = res.json();
    assert_eq!(view["profile"]["email"], email_a);
    assert_eq!(view["profile"]["role"], "user");
    assert_eq!(view["plan"]["code"], "free");
    assert_eq!(view["recent_jobs"].as_array().unwrap().len(), 1);
    assert_eq!(view["recent_jobs"][0]["id"], job_id.to_string());
    assert_eq!(view["sessions"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn support_writes_forbidden_and_change_nothing() {
    // Every write route answers 403 `PF-1002` to support —
    // and the denial changes nothing: the suspend target
    // stays active, the restore target stays suspended, the
    // impersonate attempt mints no token and writes no audit
    // row. The stub writes never run (403 before 501).
    let app = support_app().await;
    let (_support_id, support_token) = make_role(&app, &unique_email("support"), "support").await;
    let (admin_id, admin_token) = make_role(&app, &unique_email("admin"), "admin").await;
    let _ = admin_id;
    let (suspend_target, _) = make_role(&app, &unique_email("target-s"), "user").await;
    let (restore_target, _) = make_role(&app, &unique_email("target-r"), "user").await;
    let (impersonate_target, _) = make_role(&app, &unique_email("target-i"), "user").await;
    let (job_owner, _) = make_role(&app, &unique_email("job-owner"), "user").await;
    let job_id = seed_job(&app, job_owner).await;

    // The restore target must be suspended first — by an
    // admin, so the support attempt below is the only
    // restore attempt on it.
    let res = probe(
        &app,
        "POST",
        "/v1/admin/users/{id}/suspend",
        &admin_token,
        Some(restore_target),
        Some(json!({ "reason": "support readonly setup" })),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(user_status(&app, restore_target).await, "suspended");

    for row in WRITE_ROWS {
        let (target, body) = match row.target {
            "suspend" => (
                Some(suspend_target),
                Some(json!({ "reason": "support probe" })),
            ),
            "restore" => (Some(restore_target), None),
            "impersonate" => (
                Some(impersonate_target),
                Some(json!({ "reason": "support probe" })),
            ),
            "job" => (Some(job_id), None),
            _ => (None, Some(json!({}))),
        };
        let res = probe(&app, row.method, row.path, &support_token, target, body).await;
        assert_eq!(
            res.status,
            StatusCode::FORBIDDEN,
            "{} {} (support write): {}",
            row.method,
            row.path,
            res.text()
        );
        assert_eq!(problem_code(&res), "PF-1002", "{} {}", row.method, row.path);
    }

    // State after the denials: nothing moved.
    assert_eq!(
        user_status(&app, suspend_target).await,
        "active",
        "a denied suspend must not suspend"
    );
    assert_eq!(
        audit_for_user_action(&app, suspend_target, "user.suspended")
            .await
            .len(),
        0,
        "a denied suspend audits nothing"
    );
    assert_eq!(
        user_status(&app, restore_target).await,
        "suspended",
        "a denied restore must not restore"
    );
    assert_eq!(
        audit_for_user_action(&app, restore_target, "user.restored")
            .await
            .len(),
        0,
        "a denied restore audits nothing"
    );
    assert_eq!(
        audit_for_user_action(&app, impersonate_target, "user.impersonated")
            .await
            .len(),
        0,
        "a denied impersonate audits nothing"
    );
}

#[tokio::test]
async fn support_reads_stub_views() {
    // Support is admitted on the 4 stub read routes too:
    // 501 `PF-5004` — the guard passed, the handler has not
    // landed yet. A 403 here would mean the role table lost
    // support on a read; a 200 would mean a stub answered.
    let app = support_app().await;
    let (_support_id, support_token) = make_role(&app, &unique_email("support"), "support").await;
    for (method, path) in STUB_READ_ROWS {
        let res = get_as(&app, path, &support_token).await;
        assert_eq!(
            res.status,
            StatusCode::NOT_IMPLEMENTED,
            "{method} {path} (support read): {}",
            res.text()
        );
        assert_eq!(problem_code(&res), "PF-5004", "{method} {path}");
    }
}
