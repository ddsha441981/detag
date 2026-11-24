//! P1-048: role escalation tests — every way up the ladder
//! is closed, and the closed door leaves no trace in the
//! audit trail.
//!
//! The spec's "all 403 + audited" reads, under the rule
//! P1-043/P1-046 settled ("the trail records actions, not
//! attempts"), as: the attempt is 403 `PF-1002` AND the
//! audit behaviour is asserted — a denial writes NO audit
//! row. A denial that wrote a row would contradict the
//! admin guide and the `denied_and_stub_attempts_audit_nothing`
//! proof; this suite pins the negative on every write route,
//! for every caller kind that must not climb. The four tests:
//!
//! - `user_to_admin_endpoints_all_403_and_audit_nothing` —
//!   a plain user tries all 28 admin routes: 403 everywhere,
//!   no audit row for any of the 14 write actions with the
//!   user as actor, and the live-write targets are untouched
//!   (a denied suspend leaves the target `active`, a denied
//!   restore leaves a suspended target `suspended`, a denied
//!   impersonate mints no token row).
//! - `support_to_writes_all_403_and_audit_nothing` — support
//!   is read-only: 403 on all 14 writes, no audit row for any
//!   of them, same state proofs as the user test. (Support
//!   reads are the P1-044 contract — 200 on the live reads,
//!   501 on the stub reads — and are not re-proven here.)
//! - `api_key_to_admin_all_403_and_audit_nothing` — an admin
//!   user's own full-scope key tries all 28 admin routes:
//!   403 everywhere ("no admin ever", P1-040), no audit row
//!   for any write action with the key's owner as actor.
//! - `tampered_role_claim_is_rejected` — a plain user's token
//!   with its `role` claim rewritten to `admin` (payload
//!   re-encoded, original signature kept) is 401 `PF-1010`
//!   on a read and on a write: the signature fails, so the
//!   forged role never reaches a guard. The role is never
//!   trusted from the token at all — it is re-read from the
//!   user row on every request (P1-022) — so forging the
//!   claim buys nothing even before the signature check.

mod common;

use axum::http::{Method, Request, StatusCode};
use common::{TestApp, TestFixtures};
use detag_core::repos::audit_logs;
use serde_json::{json, Value};
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
async fn escalation_app() -> TestApp {
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
/// secret.
async fn create_key(app: &TestApp, token: &str) -> String {
    let body = json!({
        "name": "escalation probe",
        "scopes": ["jobs:read", "jobs:write", "webhooks:manage"],
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

/// One escalation probe: the route with a Bearer credential,
/// an optional concrete `{id}`, and an optional JSON body
/// (the write rows need `{"reason": ...}` so an admitted
/// caller reaches the handler instead of a body rejection).
async fn probe(
    app: &TestApp,
    method: &str,
    path: &str,
    credential: &str,
    target: Option<Uuid>,
    body: Option<Value>,
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
    req = req.header("authorization", format!("Bearer {credential}"));
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

/// One write route: the route and the audit action a
/// successful call would write (the target type rides the
/// same table in `apps/api/src/authz.rs`, but the
/// audit-nothing assertions here are scoped by actor, so
/// this row does not need it).
struct WriteRow {
    method: &'static str,
    path: &'static str,
    action: &'static str,
}

/// All 14 admin writes, with the audit action from
/// `Capability::audit_action_for` (`apps/api/src/authz.rs`)
/// — the same table the admin guide renders.
const WRITE_ROWS: &[WriteRow] = &[
    WriteRow {
        method: "POST",
        path: "/v1/admin/users/{id}/suspend",
        action: "user.suspended",
    },
    WriteRow {
        method: "POST",
        path: "/v1/admin/users/{id}/restore",
        action: "user.restored",
    },
    WriteRow {
        method: "POST",
        path: "/v1/admin/users/{id}/warn",
        action: "user.warned",
    },
    WriteRow {
        method: "POST",
        path: "/v1/admin/users/{id}/terminate",
        action: "user.terminated",
    },
    WriteRow {
        method: "POST",
        path: "/v1/admin/users/{id}/quota-grant",
        action: "admin.users.quota_granted",
    },
    WriteRow {
        method: "POST",
        path: "/v1/admin/users/{id}/impersonate",
        action: "user.impersonated",
    },
    WriteRow {
        method: "POST",
        path: "/v1/admin/jobs/{id}/reprocess",
        action: "admin.jobs.reprocessed",
    },
    WriteRow {
        method: "POST",
        path: "/v1/admin/jobs/{id}/cancel",
        action: "admin.jobs.cancelled",
    },
    WriteRow {
        method: "PUT",
        path: "/v1/admin/flags",
        action: "admin.flags.updated",
    },
    WriteRow {
        method: "PUT",
        path: "/v1/admin/plans",
        action: "admin.plans.updated",
    },
    WriteRow {
        method: "POST",
        path: "/v1/admin/billing/catalog-sync",
        action: "admin.billing.catalog_synced",
    },
    WriteRow {
        method: "POST",
        path: "/v1/admin/billing/subscriptions/{id}/cancel",
        action: "billing.subscription.status_changed",
    },
    WriteRow {
        method: "POST",
        path: "/v1/admin/billing/subscriptions/{id}/refund",
        action: "billing.refund.requested",
    },
    WriteRow {
        method: "POST",
        path: "/v1/admin/abuse-reports/{id}/resolve",
        action: "admin.abuse_reports.resolved",
    },
];

/// All 28 admin routes (method + path), from the matrix.
const ALL_ROUTES: &[(&str, &str)] = &[
    ("GET", "/v1/admin/metrics/dashboard"),
    ("GET", "/v1/admin/users"),
    ("GET", "/v1/admin/users/{id}"),
    ("GET", "/v1/admin/users/lookup"),
    ("POST", "/v1/admin/users/{id}/suspend"),
    ("POST", "/v1/admin/users/{id}/restore"),
    ("POST", "/v1/admin/users/{id}/warn"),
    ("POST", "/v1/admin/users/{id}/terminate"),
    ("POST", "/v1/admin/users/{id}/quota-grant"),
    ("POST", "/v1/admin/users/{id}/impersonate"),
    ("GET", "/v1/admin/jobs"),
    ("POST", "/v1/admin/jobs/{id}/reprocess"),
    ("POST", "/v1/admin/jobs/{id}/cancel"),
    ("GET", "/v1/admin/dlq"),
    ("GET", "/v1/admin/flags"),
    ("PUT", "/v1/admin/flags"),
    ("GET", "/v1/admin/plans"),
    ("PUT", "/v1/admin/plans"),
    ("POST", "/v1/admin/billing/catalog-sync"),
    ("POST", "/v1/admin/billing/subscriptions/{id}/cancel"),
    ("POST", "/v1/admin/billing/subscriptions/{id}/refund"),
    ("GET", "/v1/admin/usage/{id}"),
    ("GET", "/v1/admin/usage/{id}/export"),
    ("GET", "/v1/admin/audit"),
    ("GET", "/v1/admin/abuse-reports"),
    ("POST", "/v1/admin/abuse-reports/{id}/resolve"),
    ("GET", "/v1/admin/attestations"),
    ("GET", "/v1/admin/system"),
];

/// The target + body a write row needs: the user-scoped
/// writes take a real user id, the job/abuse writes a fresh
/// UUID (no row needs to exist — the guard rejects before
/// any handler reads it), flags/plans take none. Write
/// bodies carry a reason so an admitted caller reaches the
/// handler instead of a body rejection.
fn write_target_and_body(row: &WriteRow, user_target: Uuid) -> (Option<Uuid>, Option<Value>) {
    match row.path {
        "/v1/admin/users/{id}/suspend"
        | "/v1/admin/users/{id}/warn"
        | "/v1/admin/users/{id}/terminate"
        | "/v1/admin/users/{id}/impersonate" => (
            Some(user_target),
            Some(json!({ "reason": "escalation probe" })),
        ),
        "/v1/admin/users/{id}/restore" => (Some(user_target), None),
        "/v1/admin/users/{id}/quota-grant" => (
            Some(user_target),
            Some(json!({
                "images": 5,
                "ttl_hours": 24,
                "reason": "escalation probe"
            })),
        ),
        "/v1/admin/flags" => (None, Some(json!({ "flags": {} }))),
        "/v1/admin/plans" => (None, Some(json!({ "plans": [] }))),
        _ => (Some(Uuid::now_v7()), None),
    }
}

/// No audit row for `action` names `actor` — the denial
/// audit-nothing proof. Scoped by actor, not by absolute
/// count: the harness DB persists across runs (the P1-046
/// lesson), so a leftover row from an earlier run must not
/// fail this one.
async fn assert_no_audit_row_for_actor(app: &TestApp, action: &str, actor: Uuid) {
    let rows = audit_logs::list_for_action(&app.pool, action)
        .await
        .expect("list audit rows");
    let own: Vec<_> = rows
        .iter()
        .filter(|row| row.actor_user_id == Some(actor))
        .collect();
    assert_eq!(
        own.len(),
        0,
        "denied {action} must audit nothing (found {} row(s) by this actor)",
        own.len()
    );
}

/// The `status` of one user row, as stored.
async fn user_status(app: &TestApp, user_id: Uuid) -> String {
    sqlx::query_scalar::<_, String>("SELECT status FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_one(&app.pool)
        .await
        .expect("read user status")
}

#[tokio::test]
async fn user_to_admin_endpoints_all_403_and_audit_nothing() {
    // A plain user tries every admin route: 403 everywhere,
    // no audit row for any write, and the live-write targets
    // are untouched.
    let app = escalation_app().await;
    let (user_id, user_token) = make_role(&app, &unique_email("user"), "user").await;
    let (_admin_id, admin_token) = make_role(&app, &unique_email("admin"), "admin").await;
    // One sacrificial target per live user write, plus a
    // suspended target for the restore proof (suspended by
    // the admin first, so a denied restore must leave it
    // suspended).
    let (suspend_target, _) = make_role(&app, &unique_email("target-s"), "user").await;
    let (restore_target, _) = make_role(&app, &unique_email("target-r"), "user").await;
    let (impersonate_target, _) = make_role(&app, &unique_email("target-i"), "user").await;
    let res = probe(
        &app,
        "POST",
        "/v1/admin/users/{id}/suspend",
        &admin_token,
        Some(restore_target),
        Some(json!({ "reason": "escalation setup" })),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(user_status(&app, restore_target).await, "suspended");

    for (method, path) in ALL_ROUTES {
        let res = probe(&app, method, path, &user_token, None, None).await;
        assert_eq!(
            res.status,
            StatusCode::FORBIDDEN,
            "{method} {path} (plain user)"
        );
        assert_eq!(problem_code(&res), "PF-1002", "{method} {path}");
    }
    for row in WRITE_ROWS {
        let (target, body) = write_target_and_body(row, suspend_target);
        let res = probe(&app, row.method, row.path, &user_token, target, body).await;
        assert_eq!(
            res.status,
            StatusCode::FORBIDDEN,
            "{} {} (plain user write)",
            row.method,
            row.path
        );
        assert_eq!(problem_code(&res), "PF-1002", "{} {}", row.method, row.path);
        assert_no_audit_row_for_actor(&app, row.action, user_id).await;
    }
    // State proofs: the denied suspend left its target
    // active, the denied restore left the suspended target
    // suspended, the denied impersonate minted no row.
    assert_eq!(user_status(&app, suspend_target).await, "active");
    assert_eq!(user_status(&app, restore_target).await, "suspended");
    let rows = audit_logs::list_for_target(&app.pool, "user", &impersonate_target.to_string())
        .await
        .expect("list audit rows");
    assert!(
        rows.iter().all(|row| row.action != "user.impersonated"),
        "a denied impersonate mints no token row"
    );
}

#[tokio::test]
async fn support_to_writes_all_403_and_audit_nothing() {
    // Support is read-only: 403 on all 14 writes, no audit
    // row for any of them, same state proofs as the user
    // test. (Support reads are the P1-044 contract and are
    // not re-proven here.)
    let app = escalation_app().await;
    let (support_id, support_token) = make_role(&app, &unique_email("support"), "support").await;
    let (_admin_id, admin_token) = make_role(&app, &unique_email("admin"), "admin").await;
    let (suspend_target, _) = make_role(&app, &unique_email("target-s"), "user").await;
    let (restore_target, _) = make_role(&app, &unique_email("target-r"), "user").await;
    let (impersonate_target, _) = make_role(&app, &unique_email("target-i"), "user").await;
    let res = probe(
        &app,
        "POST",
        "/v1/admin/users/{id}/suspend",
        &admin_token,
        Some(restore_target),
        Some(json!({ "reason": "escalation setup" })),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());

    for row in WRITE_ROWS {
        let (target, body) = write_target_and_body(row, suspend_target);
        let res = probe(&app, row.method, row.path, &support_token, target, body).await;
        assert_eq!(
            res.status,
            StatusCode::FORBIDDEN,
            "{} {} (support write)",
            row.method,
            row.path
        );
        assert_eq!(problem_code(&res), "PF-1002", "{} {}", row.method, row.path);
        assert_no_audit_row_for_actor(&app, row.action, support_id).await;
    }
    assert_eq!(user_status(&app, suspend_target).await, "active");
    assert_eq!(user_status(&app, restore_target).await, "suspended");
    let rows = audit_logs::list_for_target(&app.pool, "user", &impersonate_target.to_string())
        .await
        .expect("list audit rows");
    assert!(
        rows.iter().all(|row| row.action != "user.impersonated"),
        "a denied impersonate mints no token row"
    );
}

#[tokio::test]
async fn api_key_to_admin_all_403_and_audit_nothing() {
    // An admin user's own full-scope key tries all 28 admin
    // routes: 403 everywhere ("no admin ever", P1-040), no
    // audit row for any write with the key's owner as actor.
    let app = escalation_app().await;
    let (admin_id, admin_token) = make_role(&app, &unique_email("admin"), "admin").await;
    // P1-055: key creation is plan-gated — the admin user
    // needs a Pro grant to mint the probe key.
    app.grant_plan(admin_id, "pro").await;
    let admin_key = create_key(&app, &admin_token).await;
    let (suspend_target, _) = make_role(&app, &unique_email("target-s"), "user").await;

    for (method, path) in ALL_ROUTES {
        let res = probe(&app, method, path, &admin_key, None, None).await;
        assert_eq!(
            res.status,
            StatusCode::FORBIDDEN,
            "{method} {path} (admin key)"
        );
        assert_eq!(problem_code(&res), "PF-1002", "{method} {path}");
    }
    for row in WRITE_ROWS {
        let (target, body) = write_target_and_body(row, suspend_target);
        let res = probe(&app, row.method, row.path, &admin_key, target, body).await;
        assert_eq!(
            res.status,
            StatusCode::FORBIDDEN,
            "{} {} (admin key write)",
            row.method,
            row.path
        );
        assert_eq!(problem_code(&res), "PF-1002", "{} {}", row.method, row.path);
        assert_no_audit_row_for_actor(&app, row.action, admin_id).await;
    }
    assert_eq!(user_status(&app, suspend_target).await, "active");
}

/// Base64url decode (no padding in JWT segments).
fn b64url_decode(segment: &str) -> Vec<u8> {
    let mut padded = segment.replace('-', "+").replace('_', "/");
    while !padded.len().is_multiple_of(4) {
        padded.push('=');
    }
    // A tiny decoder: the test only ever decodes a JWT
    // payload, so a hand-rolled one keeps this file free of
    // a new dependency.
    let alphabet: Vec<u8> = (0..64)
        .map(|i| b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"[i])
        .collect();
    let mut out = Vec::new();
    let bytes = padded.as_bytes();
    for chunk in bytes.chunks(4) {
        let mut vals = [0u8; 4];
        for (i, &b) in chunk.iter().enumerate() {
            vals[i] = alphabet.iter().position(|&a| a == b).unwrap_or(0) as u8;
        }
        out.push((vals[0] << 2) | (vals[1] >> 4));
        if chunk.len() > 2 {
            out.push((vals[1] << 4) | (vals[2] >> 2));
        }
        if chunk.len() > 3 {
            out.push((vals[2] << 6) | vals[3]);
        }
    }
    // Drop the bytes the `=` padding implied.
    let pad = padded.bytes().rev().take_while(|&b| b == b'=').count();
    out.truncate(out.len().saturating_sub(pad));
    out
}

/// Base64url encode without padding.
fn b64url_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[(n >> 6) as usize & 63] as char);
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[n as usize & 63] as char);
        }
    }
    out
}

/// Rewrite one claim in a JWT's payload, keeping the header
/// and the signature untouched — the forged token.
fn tamper_claim(token: &str, claim: &str, value: &str) -> String {
    let mut parts = token.splitn(3, '.');
    let header = parts.next().expect("jwt header");
    let payload = parts.next().expect("jwt payload");
    let signature = parts.next().expect("jwt signature");
    let mut claims: Value =
        serde_json::from_slice(&b64url_decode(payload)).expect("jwt payload is JSON");
    claims[claim] = Value::String(value.to_string());
    format!(
        "{}.{}.{}",
        header,
        b64url_encode(&serde_json::to_vec(&claims).unwrap()),
        signature
    )
}

#[tokio::test]
async fn tampered_role_claim_is_rejected() {
    // A plain user's token with `role` rewritten to `admin`
    // (payload re-encoded, original signature kept) is 401
    // `PF-1010` on a read and on a write: the signature
    // fails, so the forged role never reaches a guard. The
    // same holds for a role forged to `support` on a write.
    let app = escalation_app().await;
    let (_user_id, user_token) = make_role(&app, &unique_email("user"), "user").await;
    let (target, _) = make_role(&app, &unique_email("target"), "user").await;

    let forged_admin = tamper_claim(&user_token, "role", "admin");
    assert_ne!(forged_admin, user_token, "the tamper must change the token");
    let res = probe(&app, "GET", "/v1/admin/users", &forged_admin, None, None).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-1010");
    let res = probe(
        &app,
        "POST",
        "/v1/admin/users/{id}/suspend",
        &forged_admin,
        Some(target),
        Some(json!({ "reason": "forged role" })),
    )
    .await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-1010");

    let forged_support = tamper_claim(&user_token, "role", "support");
    let res = probe(
        &app,
        "POST",
        "/v1/admin/users/{id}/suspend",
        &forged_support,
        Some(target),
        Some(json!({ "reason": "forged role" })),
    )
    .await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-1010");
    // And the untampered token still behaves as a plain
    // user: 403, not 401 — the tamper test did not break
    // the real credential.
    let res = probe(&app, "GET", "/v1/admin/users", &user_token, None, None).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-1002");
}
