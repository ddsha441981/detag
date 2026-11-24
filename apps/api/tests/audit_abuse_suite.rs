//! P1-092: the audit/abuse suite — matrix-driven proof that
//! every privileged action is actually audited (FR-C-03),
//! the abuse lifecycle end to end, and the NFR-40 retention
//! boundaries.
//!
//! The audit service (P1-087) validates shape only — a
//! well-formed but wrong action still lands — so this suite
//! is the check that every privileged action actually writes
//! its row (ADR-0051/0052/0054 all point here). It drives
//! the actions through the live HTTP app, never through the
//! service layer directly, and asserts the audit row (action,
//! actor, target, before/after, reason) each one leaves.
//!
//! The four tests:
//!
//! - `admin_write_matrix_audits_every_live_action` — the 11
//!   live admin writes (suspend / restore / warn /
//!   terminate / impersonate / quota-grant / plans /
//!   billing-cancel / billing-refund / abuse-resolve dismiss
//!   / abuse-resolve start_review)
//!   each write exactly one audit row with the right action,
//!   actor and target. The registry is the 14-row
//!   `audit_action_for` table (P1-046); this test walks
//!   the 11 live rows, the stub test walks the other 3.
//! - `admin_stub_and_unconfigured_writes_audit_nothing` —
//!   the 3 stub writes (jobs reprocess / jobs cancel /
//!   flags) answer 501 and write no row for
//!   their named action; catalog-sync answers 503
//!   (unconfigured — no Stripe key in this harness) and
//!   writes no `admin.billing.catalog_synced` row; a denied
//!   write (support warn, 403) writes nothing. The trail
//!   records actions, not attempts (P1-046).
//! - `abuse_lifecycle_audit_trail_in_order` — one report
//!   walked the whole lifecycle through the public intake
//!   and the admin queue (file → review → reject_job),
//!   a second report resolved by suspending its user, a
//!   third dismissed (twice — the second dismiss is a
//!   200 no-op with no second row), and the enforcement
//!   ladder's last rung on the suspended user — with the
//!   ordered audit trail and the SLA fields asserted at
//!   each step.
//! - `retention_prunes_hold_the_nfr40_boundaries` — the
//!   NFR-40 schedules as the suite can reach them: the
//!   artifact-expiry read returns exactly the expired keys
//!   (the sweep's read leg; the user/session purges are
//!   live-tested in `apps/gc/src/purge.rs`), and the rows
//!   the missing prunes would consume — audit rows older
//!   than 365 d, usage counters older than 400 d, sent
//!   outbox rows older than 30 d — are still present, with
//!   the candidate set for each future prune exactly as
//!   expected. No prune job exists for those three yet:
//!   they are P5-017's scope (ASSUMPTION-053), and this
//!   test pins the boundary so the gap stays visible
//!   instead of silently widening.
//!
//! The harness DB persists across runs (the P1-009 lesson),
//! so every count assertion is a DELTA (before vs after),
//! never an absolute count.

mod common;

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use chrono::{DateTime, Duration, Utc};
use common::{TestApp, TestFixtures};
use detag_core::checksum::sha256_hex;
use detag_core::repos::{
    artifacts,
    audit_logs::{self, AuditLog},
    jobs::{self, NewJob},
    users,
};
use serde_json::json;
use uuid::Uuid;

/// The request id: a valid UUID, echoed by the middleware.
const REQUEST_ID: &str = "123e4567-e89b-42d3-a456-426614174109";

/// The password every test user registers with.
const PASSWORD: &str = "correct horse battery staple";

/// Test settings: the same shape as the authz matrix (generous
/// key bucket — this suite asserts audit outcomes, not rate
/// limits).
async fn suite_app() -> TestApp {
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
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
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
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
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

/// The audit rows for one target type + id + action, oldest
/// first (`list_for_target` orders by created_at ASC).
async fn audit_for_target_action(
    app: &TestApp,
    target_type: &str,
    target_id: &str,
    action: &str,
) -> Vec<AuditLog> {
    audit_logs::list_for_target(&app.pool, target_type, target_id)
        .await
        .expect("list audit rows")
        .into_iter()
        .filter(|row| row.action == action)
        .collect()
}

/// The audit rows for one action, oldest first.
async fn audit_for_action(app: &TestApp, action: &str) -> Vec<AuditLog> {
    audit_logs::list_for_action(&app.pool, action)
        .await
        .expect("list audit rows")
}

/// One admin write through the real app: method + uri +
/// bearer token + optional JSON body.
async fn admin_call(
    app: &TestApp,
    method: Method,
    uri: &str,
    token: &str,
    body: Option<serde_json::Value>,
) -> common::TestResponse {
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff())
        .header("authorization", format!("Bearer {token}"));
    if body.is_some() {
        req = req.header("content-type", "application/json");
    }
    let body = match body {
        Some(value) => Body::from(serde_json::to_vec(&value).unwrap()),
        None => Body::empty(),
    };
    app.request(req.body(body).unwrap()).await
}

/// Seed one queued image job for a fresh user; return
/// (user id, job id). A job's `upload_id` is nullable, so
/// no upload is needed.
async fn seed_job(app: &TestApp) -> (Uuid, Uuid) {
    let user = app.seed_user_unique().await;
    let job = jobs::create(
        &app.pool,
        &NewJob {
            user_id: user.id,
            upload_id: None,
            kind: "image",
            mode: "inpaint",
            status: "queued",
            lane: "standard",
            params: json!({}),
            idempotency_key: Some(&format!("p092-{}", Uuid::now_v7())),
            key_id: None,
        },
    )
    .await
    .expect("seed job");
    (user.id, job.id)
}

/// Seed an active Stripe subscription row for a user (the
/// P1-071 pattern — the provider rows are never created by
/// the API in this harness).
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
    .bind(format!("cus_p092_{}", Uuid::now_v7()))
    .bind(format!("sub_p092_{}", Uuid::now_v7()))
    .fetch_one(&app.pool)
    .await
    .expect("seed subscription")
}

/// Seed one open report naming `user_id` directly (the
/// P1-090 precedent — the queue tests need explicit
/// statuses, and no production caller files user-targeted
/// reports except through the service).
async fn seed_report_for_user(app: &TestApp, user_id: Uuid) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO abuse_reports (user_id, reporter_contact, job_id, reason, status, created_at) \
         VALUES ($1, 'reporter@example.com', NULL, 'P1-092 probe report', 'open', $2) RETURNING id",
    )
    .bind(user_id)
    .bind(Utc::now())
    .fetch_one(&app.pool)
    .await
    .expect("seed abuse report")
}

/// File a report through the public intake against a job;
/// return the report id.
async fn file_report_via_http(app: &TestApp, job_id: Uuid, contact: &str) -> Uuid {
    let body = json!({
        "job_id": job_id,
        "reporter_contact": contact,
        "reason": "Stolen artwork, please review.",
    });
    let res = app
        .request(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/abuse")
                .header("x-request-id", REQUEST_ID)
                .header("x-forwarded-for", unique_xff())
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.text());
    res.json()["id"]
        .as_str()
        .expect("ticket carries an id")
        .parse()
        .expect("ticket id is a UUID")
}

/// POST /v1/admin/abuse-reports/{id}/resolve with the given
/// action + reason.
async fn post_resolve(
    app: &TestApp,
    report_id: Uuid,
    action: &str,
    token: &str,
) -> common::TestResponse {
    admin_call(
        app,
        Method::POST,
        &format!("/v1/admin/abuse-reports/{report_id}/resolve"),
        token,
        Some(json!({ "action": action, "reason": "P1-092 suite probe" })),
    )
    .await
}

/// The current status of a user, read from the repo.
async fn user_status(app: &TestApp, user_id: Uuid) -> String {
    users::by_id(&app.pool, user_id)
        .await
        .expect("read user")
        .expect("user exists")
        .status
}

#[tokio::test]
async fn admin_write_matrix_audits_every_live_action() {
    let app = suite_app().await;
    let (admin_id, admin_token) = make_role(&app, &unique_email("admin"), "admin").await;
    let reason = json!({ "reason": "P1-092 suite probe" });

    // Suspend: exactly one new `user.suspended` row — actor
    // the admin, target the user, before `active` → after
    // `suspended`, reason verbatim.
    let (u1, _) = make_role(&app, &unique_email("target-s"), "user").await;
    let before = audit_for_target_action(&app, "user", &u1.to_string(), "user.suspended")
        .await
        .len();
    let res = admin_call(
        &app,
        Method::POST,
        &format!("/v1/admin/users/{u1}/suspend"),
        &admin_token,
        Some(reason.clone()),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let rows = audit_for_target_action(&app, "user", &u1.to_string(), "user.suspended").await;
    assert_eq!(rows.len(), before + 1, "suspend writes exactly one row");
    let row = &rows[rows.len() - 1];
    assert_eq!(row.actor_user_id, Some(admin_id), "actor is the admin");
    assert_eq!(row.target_type, "user");
    assert_eq!(row.target_id.as_deref(), Some(u1.to_string().as_str()));
    assert_eq!(row.before.as_ref().unwrap()["status"], "active");
    assert_eq!(row.after.as_ref().unwrap()["status"], "suspended");
    assert_eq!(row.reason.as_deref(), Some("P1-092 suite probe"));

    // Restore: exactly one new `user.restored` row — before
    // `suspended` → after `active`.
    let before = audit_for_target_action(&app, "user", &u1.to_string(), "user.restored")
        .await
        .len();
    let res = admin_call(
        &app,
        Method::POST,
        &format!("/v1/admin/users/{u1}/restore"),
        &admin_token,
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let rows = audit_for_target_action(&app, "user", &u1.to_string(), "user.restored").await;
    assert_eq!(rows.len(), before + 1, "restore writes exactly one row");
    let row = &rows[rows.len() - 1];
    assert_eq!(row.actor_user_id, Some(admin_id), "actor is the admin");
    assert_eq!(row.before.as_ref().unwrap()["status"], "suspended");
    assert_eq!(row.after.as_ref().unwrap()["status"], "active");

    // Warn: exactly one new `user.warned` row — the warning
    // count moves 0 → 1 in before/after.
    let (u2, _) = make_role(&app, &unique_email("target-w"), "user").await;
    let before = audit_for_target_action(&app, "user", &u2.to_string(), "user.warned")
        .await
        .len();
    let res = admin_call(
        &app,
        Method::POST,
        &format!("/v1/admin/users/{u2}/warn"),
        &admin_token,
        Some(reason.clone()),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let rows = audit_for_target_action(&app, "user", &u2.to_string(), "user.warned").await;
    assert_eq!(rows.len(), before + 1, "warn writes exactly one row");
    let row = &rows[rows.len() - 1];
    assert_eq!(row.actor_user_id, Some(admin_id), "actor is the admin");
    assert_eq!(row.before.as_ref().unwrap()["warnings"], 0);
    assert_eq!(row.after.as_ref().unwrap()["warnings"], 1);
    assert_eq!(row.reason.as_deref(), Some("P1-092 suite probe"));

    // Terminate needs a suspended user (the ladder): suspend
    // u2, then terminate — exactly one new `user.terminated`
    // row, before `suspended` → after `deletion_pending`
    // (terminate requests the 30-day deletion grace; the
    // ACTION is `user.terminated`, the status becomes
    // `deletion_pending`).
    let res = admin_call(
        &app,
        Method::POST,
        &format!("/v1/admin/users/{u2}/suspend"),
        &admin_token,
        Some(reason.clone()),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let before = audit_for_target_action(&app, "user", &u2.to_string(), "user.terminated")
        .await
        .len();
    let res = admin_call(
        &app,
        Method::POST,
        &format!("/v1/admin/users/{u2}/terminate"),
        &admin_token,
        Some(reason.clone()),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let rows = audit_for_target_action(&app, "user", &u2.to_string(), "user.terminated").await;
    assert_eq!(rows.len(), before + 1, "terminate writes exactly one row");
    let row = &rows[rows.len() - 1];
    assert_eq!(row.actor_user_id, Some(admin_id), "actor is the admin");
    assert_eq!(row.before.as_ref().unwrap()["status"], "suspended");
    assert_eq!(row.after.as_ref().unwrap()["status"], "deletion_pending");

    // Impersonate: exactly one new `user.impersonated` row —
    // actor the admin, reason verbatim, after carries the
    // 15-minute expiry.
    let (u3, _) = make_role(&app, &unique_email("target-i"), "user").await;
    let before = audit_for_target_action(&app, "user", &u3.to_string(), "user.impersonated")
        .await
        .len();
    let res = admin_call(
        &app,
        Method::POST,
        &format!("/v1/admin/users/{u3}/impersonate"),
        &admin_token,
        Some(reason.clone()),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let rows = audit_for_target_action(&app, "user", &u3.to_string(), "user.impersonated").await;
    assert_eq!(rows.len(), before + 1, "impersonate writes exactly one row");
    let row = &rows[rows.len() - 1];
    assert_eq!(row.actor_user_id, Some(admin_id), "actor is the admin");
    assert_eq!(row.reason.as_deref(), Some("P1-092 suite probe"));
    assert_eq!(row.after.as_ref().unwrap()["expires_in"], 900);

    // Quota grant (P1-107): exactly one new
    // `admin.users.quota_granted` row — actor the admin,
    // target the user, after carries the granted amounts +
    // expiry + grant id, reason verbatim.
    let (u6, _) = make_role(&app, &unique_email("target-g"), "user").await;
    let before =
        audit_for_target_action(&app, "user", &u6.to_string(), "admin.users.quota_granted")
            .await
            .len();
    let res = admin_call(
        &app,
        Method::POST,
        &format!("/v1/admin/users/{u6}/quota-grant"),
        &admin_token,
        Some(json!({
            "images": 5,
            "ttl_hours": 24,
            "reason": "P1-092 suite probe"
        })),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let rows =
        audit_for_target_action(&app, "user", &u6.to_string(), "admin.users.quota_granted").await;
    assert_eq!(rows.len(), before + 1, "quota grant writes exactly one row");
    let row = &rows[rows.len() - 1];
    assert_eq!(row.actor_user_id, Some(admin_id), "actor is the admin");
    assert_eq!(row.target_type, "user");
    assert_eq!(row.target_id.as_deref(), Some(u6.to_string().as_str()));
    assert_eq!(row.after.as_ref().unwrap()["images"], 5);
    assert!(row.after.as_ref().unwrap()["expires_at"].is_string());
    assert_eq!(row.reason.as_deref(), Some("P1-092 suite probe"));

    // Plans: a same-values PUT (Pro at its seeded 49900) —
    // the row is written even though nothing changes (the
    // service does not diff), and the catalog is left
    // exactly as found, so no other suite can observe the
    // edit. Exactly one new `admin.plans.updated` row,
    // target type `plans`, no target id.
    let before = audit_for_action(&app, "admin.plans.updated").await.len();
    let res = admin_call(
        &app,
        Method::PUT,
        "/v1/admin/plans",
        &admin_token,
        Some(json!({ "plans": [{ "code": "pro", "amount": 49900 }] })),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let rows = audit_for_action(&app, "admin.plans.updated").await;
    assert_eq!(rows.len(), before + 1, "plans PUT writes exactly one row");
    let row = &rows[rows.len() - 1];
    assert_eq!(row.actor_user_id, Some(admin_id), "actor is the admin");
    assert_eq!(row.target_type, "plans");
    assert_eq!(row.target_id, None, "plans has no target id");

    // Billing cancel: a seeded active subscription → 200,
    // exactly one new `billing.subscription.status_changed`
    // row for the subscription.
    let (u4, _) = make_role(&app, &unique_email("target-b"), "user").await;
    let sub_id = seed_subscription(&app, u4).await;
    let before = audit_for_target_action(
        &app,
        "subscription",
        &sub_id.to_string(),
        "billing.subscription.status_changed",
    )
    .await
    .len();
    let res = admin_call(
        &app,
        Method::POST,
        &format!("/v1/admin/billing/subscriptions/{sub_id}/cancel"),
        &admin_token,
        Some(reason.clone()),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let rows = audit_for_target_action(
        &app,
        "subscription",
        &sub_id.to_string(),
        "billing.subscription.status_changed",
    )
    .await;
    assert_eq!(
        rows.len(),
        before + 1,
        "billing cancel writes exactly one row"
    );
    let row = &rows[rows.len() - 1];
    assert_eq!(row.actor_user_id, Some(admin_id), "actor is the admin");
    assert_eq!(row.target_type, "subscription");

    // Billing refund (P1-108): the same subscription — now
    // canceled — refunds too (any status may be refunded),
    // exactly one new `billing.refund.requested` row for the
    // subscription, reason verbatim, after carries the
    // dashboard link.
    let before = audit_for_target_action(
        &app,
        "subscription",
        &sub_id.to_string(),
        "billing.refund.requested",
    )
    .await
    .len();
    let res = admin_call(
        &app,
        Method::POST,
        &format!("/v1/admin/billing/subscriptions/{sub_id}/refund"),
        &admin_token,
        Some(reason.clone()),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let rows = audit_for_target_action(
        &app,
        "subscription",
        &sub_id.to_string(),
        "billing.refund.requested",
    )
    .await;
    assert_eq!(
        rows.len(),
        before + 1,
        "billing refund writes exactly one row"
    );
    let row = &rows[rows.len() - 1];
    assert_eq!(row.actor_user_id, Some(admin_id), "actor is the admin");
    assert_eq!(row.target_type, "subscription");
    assert_eq!(
        row.target_id.as_deref(),
        Some(sub_id.to_string()).as_deref()
    );
    assert_eq!(row.reason.as_deref(), Some("P1-092 suite probe"));
    assert_eq!(
        row.after.as_ref().unwrap()["subscription_id"],
        sub_id.to_string()
    );
    assert!(
        row.after.as_ref().unwrap()["dashboard_url"]
            .as_str()
            .unwrap()
            .starts_with("https://dashboard.stripe.com/subscriptions"),
        "after carries the dashboard link"
    );

    // Abuse resolve, dismiss: exactly one new
    // `admin.abuse_reports.resolved` row, `after.action`
    // `dismiss`, actor the admin.
    let (u5, _) = make_role(&app, &unique_email("target-a"), "user").await;
    let report_id = seed_report_for_user(&app, u5).await;
    let before = audit_for_target_action(
        &app,
        "abuse_report",
        &report_id.to_string(),
        "admin.abuse_reports.resolved",
    )
    .await
    .len();
    let res = post_resolve(&app, report_id, "dismiss", &admin_token).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let rows = audit_for_target_action(
        &app,
        "abuse_report",
        &report_id.to_string(),
        "admin.abuse_reports.resolved",
    )
    .await;
    assert_eq!(rows.len(), before + 1, "dismiss writes exactly one row");
    let row = &rows[rows.len() - 1];
    assert_eq!(row.actor_user_id, Some(admin_id), "actor is the admin");
    assert_eq!(row.after.as_ref().unwrap()["action"], "dismiss");

    // Abuse resolve, start_review: the pickup writes
    // `abuse.report_reviewing`, NOT a resolved row — the
    // middleware's `admin.abuse_reports.resolved` action
    // must not fire for a non-terminal step.
    let report_id = seed_report_for_user(&app, u5).await;
    let before_reviewing = audit_for_target_action(
        &app,
        "abuse_report",
        &report_id.to_string(),
        "abuse.report_reviewing",
    )
    .await
    .len();
    let before_resolved = audit_for_target_action(
        &app,
        "abuse_report",
        &report_id.to_string(),
        "admin.abuse_reports.resolved",
    )
    .await
    .len();
    let res = post_resolve(&app, report_id, "start_review", &admin_token).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let reviewing = audit_for_target_action(
        &app,
        "abuse_report",
        &report_id.to_string(),
        "abuse.report_reviewing",
    )
    .await;
    assert_eq!(
        reviewing.len(),
        before_reviewing + 1,
        "start_review writes exactly one reviewing row"
    );
    assert_eq!(
        reviewing[reviewing.len() - 1].actor_user_id,
        Some(admin_id),
        "actor is the admin"
    );
    assert_eq!(
        audit_for_target_action(
            &app,
            "abuse_report",
            &report_id.to_string(),
            "admin.abuse_reports.resolved",
        )
        .await
        .len(),
        before_resolved,
        "a pickup writes no resolved row"
    );
}

#[tokio::test]
async fn admin_stub_and_unconfigured_writes_audit_nothing() {
    let app = suite_app().await;
    let (admin_id, admin_token) = make_role(&app, &unique_email("admin"), "admin").await;
    let (_support_id, support_token) = make_role(&app, &unique_email("support"), "support").await;
    let _ = admin_id;
    let (target, _) = make_role(&app, &unique_email("target"), "user").await;
    let (_job_user, job_id) = seed_job(&app).await;
    let reason = json!({ "reason": "P1-092 suite probe" });

    // Each stub write: 501, and the count for its named
    // action does not move. (The stubs take the matrix
    // guard first, so an admitted admin reaches the 501;
    // the audit middleware audits actions, not attempts.)
    let stub_cases = [
        (
            Method::POST,
            format!("/v1/admin/jobs/{job_id}/reprocess"),
            Some(json!({})),
            "admin.jobs.reprocessed",
        ),
        (
            Method::POST,
            format!("/v1/admin/jobs/{job_id}/cancel"),
            Some(json!({})),
            "admin.jobs.cancelled",
        ),
        (
            Method::PUT,
            "/v1/admin/flags".to_string(),
            Some(json!({})),
            "admin.flags.updated",
        ),
    ];
    for (method, uri, body, action) in stub_cases {
        let before = audit_for_action(&app, action).await.len();
        let res = admin_call(&app, method, &uri, &admin_token, body).await;
        assert_eq!(
            res.status,
            StatusCode::NOT_IMPLEMENTED,
            "{uri}: {}",
            res.text()
        );
        assert_eq!(
            audit_for_action(&app, action).await.len(),
            before,
            "{uri} audits nothing"
        );
    }

    // Catalog-sync is live code but unconfigured in this
    // harness (no Stripe key): 503, and no
    // `admin.billing.catalog_synced` row. The positive path
    // (a successful sync auditing the row) needs Stripe and
    // is not proven anywhere yet — P1-061 proved only the
    // guard + the unconfigured 503.
    let before = audit_for_action(&app, "admin.billing.catalog_synced")
        .await
        .len();
    let res = admin_call(
        &app,
        Method::POST,
        "/v1/admin/billing/catalog-sync",
        &admin_token,
        Some(json!({})),
    )
    .await;
    assert_eq!(
        res.status,
        StatusCode::SERVICE_UNAVAILABLE,
        "{}",
        res.text()
    );
    assert_eq!(
        audit_for_action(&app, "admin.billing.catalog_synced")
            .await
            .len(),
        before,
        "an unconfigured catalog-sync audits nothing"
    );

    // A denied write (support may not warn): 403, and no
    // `user.warned` row for the target.
    let before = audit_for_target_action(&app, "user", &target.to_string(), "user.warned")
        .await
        .len();
    let res = admin_call(
        &app,
        Method::POST,
        &format!("/v1/admin/users/{target}/warn"),
        &support_token,
        Some(reason),
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(
        audit_for_target_action(&app, "user", &target.to_string(), "user.warned")
            .await
            .len(),
        before,
        "a denied warn audits nothing"
    );
}

#[tokio::test]
async fn abuse_lifecycle_audit_trail_in_order() {
    let app = suite_app().await;
    let (admin_id, admin_token) = make_role(&app, &unique_email("admin"), "admin").await;

    // Report 1: filed through the public intake against a
    // real job — 201, and exactly one `abuse.report_filed`
    // row carrying the contact's hash, never the address.
    let (_job_user, job_id) = seed_job(&app).await;
    let contact = format!("reporter-{}@example.com", Uuid::now_v7());
    let report1 = file_report_via_http(&app, job_id, &contact).await;
    let filed = audit_for_target_action(
        &app,
        "abuse_report",
        &report1.to_string(),
        "abuse.report_filed",
    )
    .await;
    assert_eq!(filed.len(), 1, "filing writes exactly one row");
    assert_eq!(filed[0].target_type, "abuse_report");
    assert_eq!(filed[0].actor_client.as_deref(), Some("abuse-intake"));
    assert_eq!(
        filed[0].after.as_ref().unwrap()["email_hash"],
        sha256_hex(contact.as_bytes()),
        "audit carries the hash, not the address"
    );

    // The queue shows the report open, with the SLA due
    // exactly 72 h after filing and not breached.
    let res = admin_call(
        &app,
        Method::GET,
        "/v1/admin/abuse-reports?limit=100",
        &admin_token,
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let items = res.json()["items"].as_array().unwrap().clone();
    let item = items
        .iter()
        .find(|item| item["id"].as_str() == Some(report1.to_string().as_str()))
        .expect("report 1 is in the queue");
    assert_eq!(item["status"], "open");
    let created: DateTime<Utc> = item["created_at"]
        .as_str()
        .unwrap()
        .parse()
        .expect("created_at parses");
    let due: DateTime<Utc> = item["sla_due_at"]
        .as_str()
        .unwrap()
        .parse()
        .expect("sla_due_at parses");
    assert_eq!(due - created, Duration::hours(72), "SLA is created + 72 h");
    assert_eq!(item["sla_breached"], false);

    // Pick it up, then reject the job: the trail for the
    // report reads [filed, reviewing, resolved] in order,
    // and the job is `rejected`. The resolution row's
    // `after.action` names the REQUEST action
    // (`reject_job`); the report's `resolution` column
    // holds the outcome name (`job_rejected`).
    let res = post_resolve(&app, report1, "start_review", &admin_token).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let res = post_resolve(&app, report1, "reject_job", &admin_token).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["resolution"], "job_rejected");
    let job = jobs::by_id(&app.pool, job_id)
        .await
        .expect("read job")
        .expect("job exists");
    assert_eq!(job.status, "rejected");
    let trail = audit_logs::list_for_target(&app.pool, "abuse_report", &report1.to_string())
        .await
        .expect("read trail");
    let actions: Vec<&str> = trail.iter().map(|row| row.action.as_str()).collect();
    assert_eq!(
        actions,
        [
            "abuse.report_filed",
            "abuse.report_reviewing",
            "admin.abuse_reports.resolved"
        ],
        "the report's trail is the lifecycle in order"
    );
    assert!(
        trail
            .iter()
            .all(|row| row.actor_user_id == Some(admin_id) || row.action == "abuse.report_filed"),
        "every admin step is actor the admin"
    );
    let resolved = &trail[2];
    assert_eq!(resolved.after.as_ref().unwrap()["action"], "reject_job");
    // Report 2: filed against a user, resolved by suspending
    // them — the report resolves as `user_suspended` AND
    // the suspension is audited under its own name
    // (`user.suspended`, ADR-0054), so the `user.suspended`
    // filter finds it.
    let (u5, _) = make_role(&app, &unique_email("target-u"), "user").await;
    let report2 = seed_report_for_user(&app, u5).await;
    let res = post_resolve(&app, report2, "start_review", &admin_token).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let res = post_resolve(&app, report2, "suspend_user", &admin_token).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["resolution"], "user_suspended");
    assert_eq!(user_status(&app, u5).await, "suspended");
    let trail = audit_logs::list_for_target(&app.pool, "abuse_report", &report2.to_string())
        .await
        .expect("read trail");
    let actions: Vec<&str> = trail.iter().map(|row| row.action.as_str()).collect();
    assert_eq!(
        actions,
        ["abuse.report_reviewing", "admin.abuse_reports.resolved"],
        "the seeded report's trail starts at the pickup"
    );
    assert_eq!(trail[1].after.as_ref().unwrap()["action"], "suspend_user");
    let suspended = audit_for_target_action(&app, "user", &u5.to_string(), "user.suspended").await;
    assert_eq!(
        suspended.len(),
        1,
        "the suspension is audited by its own name"
    );
    assert_eq!(suspended[0].actor_user_id, Some(admin_id));

    // The ladder's last rung on the suspended user: warn
    // is 422 on the wrong rung, terminate is 200 and writes
    // exactly one `user.terminated` row.
    let res = admin_call(
        &app,
        Method::POST,
        &format!("/v1/admin/users/{u5}/warn"),
        &admin_token,
        Some(json!({ "reason": "P1-092 suite probe" })),
    )
    .await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    let before = audit_for_target_action(&app, "user", &u5.to_string(), "user.terminated")
        .await
        .len();
    let res = admin_call(
        &app,
        Method::POST,
        &format!("/v1/admin/users/{u5}/terminate"),
        &admin_token,
        Some(json!({ "reason": "P1-092 suite probe" })),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let rows = audit_for_target_action(&app, "user", &u5.to_string(), "user.terminated").await;
    assert_eq!(rows.len(), before + 1, "terminate writes exactly one row");
    assert_eq!(rows[rows.len() - 1].actor_user_id, Some(admin_id));

    // Report 3: dismiss, then dismiss again — the second
    // dismiss is a 200 no-op with no second resolved row
    // (the P1-071 precedent).
    let report3 = seed_report_for_user(&app, u5).await;
    let res = post_resolve(&app, report3, "dismiss", &admin_token).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let resolved = audit_for_target_action(
        &app,
        "abuse_report",
        &report3.to_string(),
        "admin.abuse_reports.resolved",
    )
    .await;
    assert_eq!(resolved.len(), 1, "dismiss writes exactly one row");
    assert_eq!(resolved[0].after.as_ref().unwrap()["action"], "dismiss");
    let res = post_resolve(&app, report3, "dismiss", &admin_token).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(
        audit_for_target_action(
            &app,
            "abuse_report",
            &report3.to_string(),
            "admin.abuse_reports.resolved",
        )
        .await
        .len(),
        1,
        "a second dismiss writes no second row"
    );
}

#[tokio::test]
async fn retention_prunes_hold_the_nfr40_boundaries() {
    let app = suite_app().await;
    let now = Utc::now();

    // Artifacts: the expiry read (the sweep's read leg) —
    // one artifact expired an hour ago, one expiring
    // tomorrow; exactly the expired key comes back. The
    // keys are unique per run — the harness DB persists
    // across runs (the P1-009 lesson), so a fixed key
    // would come back once per past run.
    let (_user, job_id) = seed_job(&app).await;
    let run_tag = Uuid::now_v7();
    let expired_key = format!("p092-{run_tag}/output.png");
    let live_key = format!("p092-{run_tag}/report.json");
    for (key, expires_at) in [
        (expired_key.as_str(), now - Duration::hours(1)),
        (live_key.as_str(), now + Duration::hours(24)),
    ] {
        sqlx::query(
            "INSERT INTO artifacts (job_id, type, storage_key, bytes, sha256, expires_at) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(job_id)
        .bind("output")
        .bind(key)
        .bind(10_i64)
        .bind("bb")
        .bind(expires_at)
        .execute(&app.pool)
        .await
        .expect("insert artifact");
    }
    let expired = artifacts::expired_storage_keys(&app.pool, now)
        .await
        .expect("expired_storage_keys");
    // Contains, not equals: past runs of this suite left
    // their own expired rows behind in the shared harness
    // DB (the P1-009 lesson) — the read leg is proven by
    // this run's key being present and the live key absent.
    assert!(
        expired.contains(&expired_key),
        "the expired key is returned: {expired:?}"
    );
    assert!(
        !expired.contains(&live_key),
        "the live key is not returned: {expired:?}"
    );

    // Audit log, 365 d: no prune job exists yet (P5-017),
    // so a row backdated 366 d is still present — and the
    // candidate set for the future prune is exactly it.
    // (Direct INSERT with an explicit created_at: the
    // append-only trigger binds UPDATE/DELETE, not INSERT.)
    let old_audit: Uuid = sqlx::query_scalar(
        "INSERT INTO audit_logs (actor_user_id, actor_client, action, target_type, target_id, \"before\", \"after\", reason, created_at) \
         VALUES (NULL, 'retention-probe', 'test.action', 'user', $1, NULL, NULL, 'P1-092 retention probe', $2) RETURNING id",
    )
    .bind(Uuid::now_v7().to_string())
    .bind(now - Duration::days(366))
    .fetch_one(&app.pool)
    .await
    .expect("insert old audit row");
    let candidates: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM audit_logs WHERE created_at < now() - interval '365 days' AND id = $1",
    )
    .bind(old_audit)
    .fetch_all(&app.pool)
    .await
    .expect("audit candidate query");
    assert_eq!(
        candidates,
        vec![old_audit],
        "the 366-day row is the prune candidate"
    );
    assert!(
        audit_logs::list_for_action(&app.pool, "test.action")
            .await
            .expect("list audit rows")
            .iter()
            .any(|row| row.id == old_audit),
        "the old row is still listed — no prune ran"
    );

    // Usage counters, 400 d: the policy row (P1-086) with no
    // enforcing purge mode yet — a day 401 d ago is still
    // present, and is exactly the future prune's candidate.
    let (counter_user, _) = make_role(&app, &unique_email("counter"), "user").await;
    let old_day = (now - Duration::days(401)).date_naive();
    sqlx::query(
        "INSERT INTO usage_counters (user_id, day, images_used, video_seconds_used) \
         VALUES ($1, $2, 5, 60)",
    )
    .bind(counter_user)
    .bind(old_day)
    .execute(&app.pool)
    .await
    .expect("insert old usage row");
    let kept: Option<i32> = sqlx::query_scalar(
        "SELECT images_used FROM usage_counters WHERE user_id = $1 AND day = $2",
    )
    .bind(counter_user)
    .bind(old_day)
    .fetch_optional(&app.pool)
    .await
    .expect("read usage row");
    assert_eq!(kept, Some(5), "the 401-day row is still present");
    let candidates: Vec<chrono::NaiveDate> = sqlx::query_scalar(
        "SELECT day FROM usage_counters WHERE user_id = $1 AND day < CURRENT_DATE - interval '400 days'",
    )
    .bind(counter_user)
    .fetch_all(&app.pool)
    .await
    .expect("usage candidate query");
    assert_eq!(
        candidates,
        vec![old_day],
        "the 401-day row is the prune candidate"
    );

    // Email outbox, sent rows, 30 d: same shape — a sent row
    // 31 d ago is still present, and is exactly the future
    // prune's candidate.
    let old_mail: Uuid = sqlx::query_scalar(
        "INSERT INTO email_outbox (to_hash, template, payload, scheduled_at, sent_at, attempts) \
         VALUES ($1, 'password-reset', '{}', $2, $2, 1) RETURNING id",
    )
    .bind(sha256_hex(b"retention-probe@example.com"))
    .bind(now - Duration::days(31))
    .fetch_one(&app.pool)
    .await
    .expect("insert old outbox row");
    let kept: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM email_outbox WHERE id = $1 AND sent_at IS NOT NULL")
            .bind(old_mail)
            .fetch_optional(&app.pool)
            .await
            .expect("read outbox row");
    assert_eq!(kept, Some(old_mail), "the 31-day sent row is still present");
    let candidates: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM email_outbox WHERE sent_at < now() - interval '30 days' AND id = $1",
    )
    .bind(old_mail)
    .fetch_all(&app.pool)
    .await
    .expect("outbox candidate query");
    assert_eq!(
        candidates,
        vec![old_mail],
        "the 31-day row is the prune candidate"
    );
}
