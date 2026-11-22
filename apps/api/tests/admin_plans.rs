//! P1-056: the admin plan editor — `GET` / `PUT`
//! `/v1/admin/plans`.
//!
//! Each test runs against its OWN scratch database
//! (`detag_api_harness_plans_<tag>`): the editor mutates
//! the shared catalog (amounts, entitlements, even deleting
//! a plan), and the other suites assert the seeded values
//! (`plans.rs` pins all three plans exactly; the quota
//! suites pin Pro's limits) — editing the shared harness
//! DB would race them. A dedicated DB per test is the
//! `crates/core/tests/job_events_retry.rs` pattern (fixed
//! name, drop-stale-first, drop at the end); the migrate
//! takes ~1 s on a fresh database, so the isolation costs
//! nothing measurable.
//!
//! The six tests:
//!
//! - `get_plans_lists_seeded_catalog` — GET lists the three
//!   seeded plans cheapest first with the FR-P-01 values.
//! - `put_updates_entitlements_and_price` — PUT edits Pro's
//!   amount, Stripe price and entitlements; a GET before the
//!   PUT populates the catalog cache, so the GET after
//!   proves the invalidation; a Pro user's `/v1/me` proves
//!   the effective plan sees the edit too.
//! - `put_validates_entitlements` — the 422 table (bad
//!   entitlements, bad price/currency, Free rules, unknown
//!   code, contradictory lists), each followed by a GET
//!   proving nothing was applied; failures write no audit row.
//! - `put_removal_blocked_with_active_subscriptions` — a
//!   live Pro subscription and a canceled Business one each
//!   block removal with 409 `PF-2012`.
//! - `put_removal_allowed_with_no_subscriptions` — the
//!   empty PUT is a no-op 200; removing Business (no subs)
//!   leaves a 2-plan catalog.
//! - `admin_plans_guard_and_audit` — the guard (support
//!   reads, admin writes, everyone else 401/403, a key 403)
//!   and the audit: one `admin.plans.updated` row per
//!   successful PUT (actor, target type `plans`, the changed
//!   codes in `after`), none for a denial or a 404.

mod common;

use axum::http::{Method, Request, StatusCode};
use common::{TestApp, TestResponse};
use detag_core::{
    config::Settings,
    db::{connect_lazy, PoolConfig},
    migrate::migrate_up,
    repos::audit_logs,
};
use serde_json::json;
use sqlx::Connection as _;
use std::path::PathBuf;
use std::time::Duration;
use uuid::Uuid;

/// The request id: a valid UUID, echoed by the middleware.
const REQUEST_ID: &str = "123e4567-e89b-42d3-a456-426614174109";

/// The password every test user registers with.
const PASSWORD: &str = "correct horse battery staple";

fn migrations_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("migrations")
}

/// The base postgres URL (the server the harness points at),
/// from the environment or the dev default.
fn base_url() -> String {
    std::env::var("PF_DATABASE_URL").unwrap_or_else(|_| Settings::default().database_url)
}

/// The URL of one scratch database on the base server: the
/// base URL keeps its user, host, port and query string;
/// only the database name changes.
fn db_url(base: &str, name: &str) -> String {
    let (head, query) = match base.split_once('?') {
        Some((head, query)) => (head, Some(query)),
        None => (base, None),
    };
    let base = match head.rfind('/') {
        Some(idx) => &head[..idx],
        None => head,
    };
    match query {
        Some(query) => format!("{base}/{name}?{query}"),
        None => format!("{base}/{name}"),
    }
}

/// Static DROP/CREATE literals per test tag (sqlx 0.9
/// forbids `format!` in `sqlx::query` — the
/// `crates/core/tests/job_events_retry.rs` pattern): one
/// literal per scratch database, matched on the tag.
fn scratch_sql(tag: &str) -> (&'static str, &'static str, &'static str) {
    match tag {
        "get" => (
            "DROP DATABASE IF EXISTS \"detag_api_harness_plans_get\"",
            "CREATE DATABASE \"detag_api_harness_plans_get\"",
            "detag_api_harness_plans_get",
        ),
        "put" => (
            "DROP DATABASE IF EXISTS \"detag_api_harness_plans_put\"",
            "CREATE DATABASE \"detag_api_harness_plans_put\"",
            "detag_api_harness_plans_put",
        ),
        "validate" => (
            "DROP DATABASE IF EXISTS \"detag_api_harness_plans_validate\"",
            "CREATE DATABASE \"detag_api_harness_plans_validate\"",
            "detag_api_harness_plans_validate",
        ),
        "remove_blocked" => (
            "DROP DATABASE IF EXISTS \"detag_api_harness_plans_remove_blocked\"",
            "CREATE DATABASE \"detag_api_harness_plans_remove_blocked\"",
            "detag_api_harness_plans_remove_blocked",
        ),
        "remove_ok" => (
            "DROP DATABASE IF EXISTS \"detag_api_harness_plans_remove_ok\"",
            "CREATE DATABASE \"detag_api_harness_plans_remove_ok\"",
            "detag_api_harness_plans_remove_ok",
        ),
        "audit" => (
            "DROP DATABASE IF EXISTS \"detag_api_harness_plans_audit\"",
            "CREATE DATABASE \"detag_api_harness_plans_audit\"",
            "detag_api_harness_plans_audit",
        ),
        _ => panic!("unknown plans scratch db {tag}"),
    }
}

/// Spawn an app on a fresh scratch database named for this
/// test (`detag_api_harness_plans_<tag>`). A stale database
/// from a killed run is dropped first, so a re-run never
/// trips on a leftover. Returns the app and the database
/// name; the test drops the database with [`drop_plans_db`]
/// when it is done.
async fn plans_app(tag: &str) -> (TestApp, &'static str) {
    let (drop_sql, create_sql, db) = scratch_sql(tag);
    let base = base_url();
    let mut admin = sqlx::PgConnection::connect(&base)
        .await
        .expect("plans harness: Postgres must be up (native `just dev` or compose)");
    sqlx::query(drop_sql)
        .execute(&mut admin)
        .await
        .expect("plans harness: drop stale scratch db");
    sqlx::query(create_sql)
        .execute(&mut admin)
        .await
        .expect("plans harness: create scratch db");
    let url = db_url(&base, db);
    let pool = connect_lazy(&url, &PoolConfig::default()).expect("plans harness: pool builds");
    migrate_up(&pool, &migrations_dir())
        .await
        .expect("plans harness: migrations apply");
    pool.close().await;
    let settings = Settings {
        database_url: url,
        disposable_email_domains: "mailinator.com".to_string(),
        tos_current_version: "v1".to_string(),
        ..Settings::default()
    };
    let app = TestApp::spawn_with(settings).await;
    (app, db)
}

/// Drop the test's scratch database. The app's pools are
/// closed first; a few retries cover connections still
/// draining after the close.
async fn drop_plans_db(app: TestApp, tag: &str) {
    app.pool.close().await;
    app.state.pool.close().await;
    let (drop_sql, _, _) = scratch_sql(tag);
    let base = base_url();
    let mut admin = sqlx::PgConnection::connect(&base)
        .await
        .expect("plans harness: Postgres must be up to drop the scratch db");
    for _ in 0..5 {
        match sqlx::query(drop_sql).execute(&mut admin).await {
            Ok(_) => return,
            Err(_) => tokio::time::sleep(Duration::from_millis(200)).await,
        }
    }
    panic!("plans harness: could not drop scratch db {tag}");
}

fn unique_email(prefix: &str) -> String {
    format!("{prefix}-{}@example.com", Uuid::now_v7())
}

/// A per-request XFF address: each request gets its own IP
/// bucket, so the tests never trip each other's rate limits
/// (the P1-003 lesson, full-UUID form — FLAKY-006).
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

/// Mint a full-scope key for the JWT holder; return the
/// secret. The caller grants the holder a plan first when
/// the key mint is plan-gated (P1-055).
async fn create_key(app: &TestApp, token: &str) -> String {
    let body =
        json!({ "name": "plans probe", "scopes": ["jobs:read", "jobs:write", "webhooks:manage"] });
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

/// One plans probe: GET or PUT `/v1/admin/plans` with an
/// optional Bearer token (`None` = anonymous) and an
/// optional JSON body.
async fn probe(
    app: &TestApp,
    method: &str,
    credential: Option<&str>,
    body: Option<serde_json::Value>,
) -> TestResponse {
    let mut req = Request::builder()
        .method(Method::from_bytes(method.as_bytes()).unwrap())
        .uri("/v1/admin/plans")
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff());
    if method == "PUT" {
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
fn problem_code(res: &TestResponse) -> String {
    res.json()["code"].as_str().unwrap_or("").to_string()
}

/// The audit rows for one action, oldest first.
async fn audit_for_action(app: &TestApp, action: &str) -> Vec<audit_logs::AuditLog> {
    audit_logs::list_for_action(&app.pool, action)
        .await
        .expect("list audit rows")
}

/// The seeded Pro entitlements, as an edit body (the full
/// seven keys — the editor requires the whole object).
fn pro_entitlements() -> serde_json::Value {
    json!({
        "daily_images": 500,
        "daily_video_seconds": 3600,
        "max_bytes": 1073741824,
        "result_ttl_hours": 24,
        "concurrency": 3,
        "lane": "standard",
        "features": {"api_keys": true, "webhooks": true}
    })
}

/// Insert a subscription row directly with an explicit
/// status — there is no billing flow yet to create one
/// through the API.
async fn seed_subscription(app: &TestApp, user_id: Uuid, plan_code: &str, status: &str) {
    let plan = detag_core::repos::plans::by_code(&app.pool, plan_code)
        .await
        .expect("load plan")
        .expect("plan is seeded");
    sqlx::query(
        "INSERT INTO subscriptions (user_id, plan_id, provider, status, created_at) \
         VALUES ($1, $2, 'stripe', $3, now())",
    )
    .bind(user_id)
    .bind(plan.id)
    .bind(status)
    .execute(&app.pool)
    .await
    .expect("seed subscription");
}

#[tokio::test]
async fn get_plans_lists_seeded_catalog() {
    let (app, _db) = plans_app("get").await;
    const TAG: &str = "get";
    let (_admin_id, admin_token) = make_role(&app, &unique_email("admin"), "admin").await;
    let res = probe(&app, "GET", Some(&admin_token), None).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let plans = res.json()["plans"].as_array().unwrap().clone();
    assert_eq!(plans.len(), 3, "exactly the three seeded plans");
    let codes: Vec<&str> = plans
        .iter()
        .map(|plan| plan["code"].as_str().unwrap())
        .collect();
    assert_eq!(codes, vec!["free", "pro", "business"], "cheapest first");

    let free = &plans[0];
    assert_eq!(free["amount"], 0);
    assert_eq!(free["currency"], "INR");
    assert!(free["stripe_price_id"].is_null(), "free has no price");
    let ent = &free["entitlements"];
    assert_eq!(ent["daily_images"], 10);
    assert_eq!(ent["daily_video_seconds"], 0);
    assert_eq!(ent["max_bytes"], 26_214_400);
    assert_eq!(ent["result_ttl_hours"], 6);
    assert_eq!(ent["concurrency"], 1);
    assert_eq!(ent["lane"], "standard");
    assert_eq!(ent["features"], json!({}));

    let pro = &plans[1];
    assert_eq!(pro["amount"], 49900);
    assert!(pro["stripe_price_id"].is_null(), "seed has no price yet");
    let ent = &pro["entitlements"];
    assert_eq!(ent["daily_images"], 500);
    assert_eq!(ent["daily_video_seconds"], 3600);
    assert_eq!(ent["max_bytes"], 1_073_741_824);
    assert_eq!(ent["result_ttl_hours"], 24);
    assert_eq!(ent["concurrency"], 3);
    assert_eq!(ent["lane"], "standard");
    assert_eq!(ent["features"], json!({"api_keys": true, "webhooks": true}));

    let business = &plans[2];
    assert_eq!(business["amount"], 199900);
    let ent = &business["entitlements"];
    assert_eq!(ent["daily_images"], 5000);
    assert_eq!(ent["daily_video_seconds"], 18000);
    assert_eq!(ent["max_bytes"], 4_294_967_296i64);
    assert_eq!(ent["result_ttl_hours"], 72);
    assert_eq!(ent["concurrency"], 10);
    assert_eq!(ent["lane"], "priority");
    assert_eq!(
        ent["features"],
        json!({"api_keys": true, "webhooks": true, "batch": true})
    );
    drop_plans_db(app, TAG).await;
}

#[tokio::test]
async fn put_updates_entitlements_and_price() {
    let (app, _db) = plans_app("put").await;
    const TAG: &str = "put";
    let (_admin_id, admin_token) = make_role(&app, &unique_email("admin"), "admin").await;
    // A GET first populates the catalog cache — the GET
    // after the PUT must show the edit, proving the PUT
    // invalidated it (the 5-minute TTL would otherwise
    // serve the stale catalog).
    let res = probe(&app, "GET", Some(&admin_token), None).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["plans"][1]["amount"], 49900);

    let mut ent = pro_entitlements();
    ent["daily_images"] = json!(600);
    let res = probe(
        &app,
        "PUT",
        Some(&admin_token),
        Some(json!({
            "plans": [{
                "code": "pro",
                "amount": 59900,
                "stripe_price_id": "price_1TestPro123",
                "entitlements": ent,
            }],
        })),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let plans = res.json()["plans"].as_array().unwrap().clone();
    assert_eq!(plans.len(), 3, "the catalog keeps all three plans");
    let pro = &plans[1];
    assert_eq!(pro["code"], "pro");
    assert_eq!(pro["amount"], 59900);
    assert_eq!(pro["stripe_price_id"], "price_1TestPro123");
    assert_eq!(pro["entitlements"]["daily_images"], 600);
    assert_eq!(pro["entitlements"]["daily_video_seconds"], 3600);

    // The GET after the PUT reads through the invalidated
    // cache: the new values, not the cached seed values.
    let res = probe(&app, "GET", Some(&admin_token), None).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let pro = &res.json()["plans"][1];
    assert_eq!(pro["amount"], 59900, "cache invalidated by the PUT");
    assert_eq!(pro["entitlements"]["daily_images"], 600);

    // And the effective plan sees the edit too: a Pro user
    // resolves to the edited price and limits.
    let email = unique_email("pro-user");
    let pro_user_id = register_user(&app, &email).await;
    app.grant_plan(pro_user_id, "pro").await;
    let user_token = login_token(&app, &email).await;
    let res = app
        .request(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/me")
                .header("x-request-id", REQUEST_ID)
                .header("x-forwarded-for", unique_xff())
                .header("authorization", format!("Bearer {user_token}"))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["plan"]["code"], "pro");
    assert_eq!(res.json()["plan"]["amount"], 59900, "edited price");
    assert_eq!(
        res.json()["quota"]["daily_images"]["limit"],
        600,
        "edited limit"
    );
    drop_plans_db(app, TAG).await;
}

#[tokio::test]
async fn put_validates_entitlements() {
    let (app, _db) = plans_app("validate").await;
    const TAG: &str = "validate";
    let (_admin_id, admin_token) = make_role(&app, &unique_email("admin"), "admin").await;
    // The failures must write no audit row at all.
    assert_eq!(audit_for_action(&app, "admin.plans.updated").await.len(), 0);

    // Each case: the PUT, the expected status + code, and —
    // after every case — a GET proving the catalog is
    // unchanged (validation failures apply nothing).
    let mut bad_ent = pro_entitlements();
    bad_ent["daily_images"] = json!(-1);
    let mut missing_key = pro_entitlements();
    missing_key.as_object_mut().unwrap().remove("concurrency");
    let mut bad_lane = pro_entitlements();
    bad_lane["lane"] = json!("express");
    let mut unknown_feature = pro_entitlements();
    unknown_feature["features"] = json!({"api_keys": true, "teleport": true});
    let cases: Vec<(serde_json::Value, StatusCode, &str)> = vec![
        // Negative quota.
        (
            json!({"plans": [{"code": "pro", "entitlements": bad_ent}]}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "PF-2002",
        ),
        // A partial entitlements object (missing concurrency).
        (
            json!({"plans": [{"code": "pro", "entitlements": missing_key}]}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "PF-2002",
        ),
        // Unknown lane.
        (
            json!({"plans": [{"code": "pro", "entitlements": bad_lane}]}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "PF-2002",
        ),
        // Unknown feature name.
        (
            json!({"plans": [{"code": "pro", "entitlements": unknown_feature}]}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "PF-2002",
        ),
        // A mistyped feature value fails serde (plain 422 —
        // no field errors, nothing parsed).
        (
            json!({"plans": [{"code": "pro", "entitlements": {
                "daily_images": 500, "daily_video_seconds": 3600,
                "max_bytes": 1073741824, "result_ttl_hours": 24,
                "concurrency": 3, "lane": "standard",
                "features": {"api_keys": "yes"}}}]}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "PF-2002",
        ),
        // A Stripe price id that is not a price id.
        (
            json!({"plans": [{"code": "pro", "stripe_price_id": "not-a-price"}]}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "PF-2002",
        ),
        // Free must stay ₹0.
        (
            json!({"plans": [{"code": "free", "amount": 49900}]}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "PF-2002",
        ),
        // Free takes no Stripe price.
        (
            json!({"plans": [{"code": "free", "stripe_price_id": "price_1abc"}]}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "PF-2002",
        ),
        // A bad currency code.
        (
            json!({"plans": [{"code": "pro", "currency": "inr"}]}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "PF-2002",
        ),
        // A negative amount.
        (
            json!({"plans": [{"code": "pro", "amount": -100}]}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "PF-2002",
        ),
        // The same code in both lists.
        (
            json!({"plans": [{"code": "pro"}], "remove": ["pro"]}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "PF-2002",
        ),
        // The same code twice in one list.
        (
            json!({"plans": [{"code": "pro"}, {"code": "pro"}]}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "PF-2002",
        ),
        // Free cannot be removed (422 — a property of the
        // request, not of the plan's state).
        (
            json!({"remove": ["free"]}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "PF-2002",
        ),
    ];
    for (index, (body, want_status, want_code)) in cases.into_iter().enumerate() {
        let res = probe(&app, "PUT", Some(&admin_token), Some(body)).await;
        assert_eq!(res.status, want_status, "case {index}: {}", res.text());
        assert_eq!(problem_code(&res), want_code, "case {index}");
        // Every case carries field_errors (explicit
        // validation) except the serde-type one (case 4: a
        // mistyped feature value fails the parse, so nothing
        // parsed and there are no fields to report).
        if index != 4 {
            assert!(
                res.json()["field_errors"]
                    .as_array()
                    .is_some_and(|errs| !errs.is_empty()),
                "case {index}: field_errors must name the bad field: {}",
                res.text()
            );
        }
        // Nothing applied: Pro is still the seed row.
        let res = probe(&app, "GET", Some(&admin_token), None).await;
        assert_eq!(res.status, StatusCode::OK, "{}", res.text());
        let pro = &res.json()["plans"][1];
        assert_eq!(pro["amount"], 49900, "case {index}: no change");
        assert_eq!(pro["entitlements"]["daily_images"], 500, "case {index}");
        assert!(pro["stripe_price_id"].is_null(), "case {index}");
    }
    // An unknown plan code is a 404, not a 422.
    let res = probe(
        &app,
        "PUT",
        Some(&admin_token),
        Some(json!({"plans": [{"code": "enterprise", "amount": 1}]})),
    )
    .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-2010");
    // An unknown removal code is a 404 too.
    let res = probe(
        &app,
        "PUT",
        Some(&admin_token),
        Some(json!({"remove": ["enterprise"]})),
    )
    .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-2010");
    // Still no audit row: every failure above changed
    // nothing, so the trail records nothing.
    assert_eq!(audit_for_action(&app, "admin.plans.updated").await.len(), 0);
    drop_plans_db(app, TAG).await;
}

#[tokio::test]
async fn put_removal_blocked_with_active_subscriptions() {
    let (app, _db) = plans_app("remove_blocked").await;
    const TAG: &str = "remove_blocked";
    let (_admin_id, admin_token) = make_role(&app, &unique_email("admin"), "admin").await;
    // A live Pro subscription blocks Pro's removal.
    let pro_user = app.seed_user_unique().await;
    app.grant_plan(pro_user.id, "pro").await;
    let res = probe(
        &app,
        "PUT",
        Some(&admin_token),
        Some(json!({"remove": ["pro"]})),
    )
    .await;
    assert_eq!(res.status, StatusCode::CONFLICT, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-2012");
    // A canceled Business subscription blocks Business too:
    // `plan_id` is ON DELETE RESTRICT, so even historical
    // subscriptions make the delete impossible at the DB.
    let business_user = app.seed_user_unique().await;
    seed_subscription(&app, business_user.id, "business", "canceled").await;
    let res = probe(
        &app,
        "PUT",
        Some(&admin_token),
        Some(json!({"remove": ["business"]})),
    )
    .await;
    assert_eq!(res.status, StatusCode::CONFLICT, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-2012");
    // The blocked PUT applied nothing: both plans are still
    // listed, with their seed values.
    let res = probe(&app, "GET", Some(&admin_token), None).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let plans = res.json()["plans"].as_array().unwrap().clone();
    assert_eq!(plans.len(), 3, "nothing was removed");
    assert_eq!(plans[1]["amount"], 49900);
    assert_eq!(plans[2]["amount"], 199900);
    drop_plans_db(app, TAG).await;
}

#[tokio::test]
async fn put_removal_allowed_with_no_subscriptions() {
    let (app, _db) = plans_app("remove_ok").await;
    const TAG: &str = "remove_ok";
    let (_admin_id, admin_token) = make_role(&app, &unique_email("admin"), "admin").await;
    // The empty PUT is a no-op 200, not an error.
    let res = probe(&app, "PUT", Some(&admin_token), Some(json!({}))).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["plans"].as_array().unwrap().len(), 3);
    let res = probe(
        &app,
        "PUT",
        Some(&admin_token),
        Some(json!({"plans": [], "remove": []})),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["plans"].as_array().unwrap().len(), 3);

    // Business has no subscriptions in this database: the
    // removal succeeds and the catalog shrinks to two.
    let res = probe(
        &app,
        "PUT",
        Some(&admin_token),
        Some(json!({"remove": ["business"]})),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let plans = res.json()["plans"].as_array().unwrap().clone();
    assert_eq!(plans.len(), 2, "business is gone");
    let codes: Vec<&str> = plans
        .iter()
        .map(|plan| plan["code"].as_str().unwrap())
        .collect();
    assert_eq!(codes, vec!["free", "pro"]);
    let res = probe(&app, "GET", Some(&admin_token), None).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["plans"].as_array().unwrap().len(), 2);
    drop_plans_db(app, TAG).await;
}

#[tokio::test]
async fn admin_plans_guard_and_audit() {
    let (app, _db) = plans_app("audit").await;
    const TAG: &str = "audit";
    let (_user_id, user_token) = make_role(&app, &unique_email("user"), "user").await;
    let (_support_id, support_token) = make_role(&app, &unique_email("support"), "support").await;
    let (admin_id, admin_token) = make_role(&app, &unique_email("admin"), "admin").await;
    // P1-055: key creation is plan-gated — the admin user
    // needs a Pro grant to mint the probe key.
    app.grant_plan(admin_id, "pro").await;
    let admin_key = create_key(&app, &admin_token).await;

    // The guard: support reads (200), nobody else reads or
    // writes. A key is 403 on both — "no admin ever".
    let res = probe(&app, "GET", Some(&support_token), None).await;
    assert_eq!(res.status, StatusCode::OK, "support read: {}", res.text());
    let res = probe(&app, "GET", Some(&user_token), None).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-1002");
    let res = probe(&app, "GET", None, None).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-1001");
    let res = probe(&app, "GET", Some(&admin_key), None).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-1002");

    let no_op = Some(json!({"plans": []}));
    let res = probe(&app, "PUT", Some(&support_token), no_op.clone()).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-1002");
    let res = probe(&app, "PUT", Some(&user_token), no_op.clone()).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    let res = probe(&app, "PUT", None, no_op.clone()).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    let res = probe(&app, "PUT", Some(&admin_key), no_op.clone()).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-1002");

    // The audit: no rows yet on this fresh database.
    assert_eq!(audit_for_action(&app, "admin.plans.updated").await.len(), 0);
    // A successful PUT writes exactly one row — actor, the
    // action, target type `plans` with no target id (the
    // route has no `{id}`), and the changed codes in `after`.
    let res = probe(
        &app,
        "PUT",
        Some(&admin_token),
        Some(json!({"plans": [{"code": "pro", "amount": 59900}]})),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let rows = audit_for_action(&app, "admin.plans.updated").await;
    assert_eq!(rows.len(), 1, "one audit row for the PUT");
    let row = &rows[0];
    assert_eq!(row.action, "admin.plans.updated");
    assert_eq!(row.actor_user_id, Some(admin_id), "actor is the admin");
    assert_eq!(row.target_type, "plans");
    assert_eq!(row.target_id, None, "plans has no target id");
    assert_eq!(row.after.as_ref().unwrap()["updated"], json!(["pro"]));
    assert_eq!(row.after.as_ref().unwrap()["removed"], json!([]));

    // A denied PUT and a 404 PUT write nothing more: the
    // trail records actions, not attempts.
    let res = probe(&app, "PUT", Some(&support_token), no_op.clone()).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    let res = probe(
        &app,
        "PUT",
        Some(&admin_token),
        Some(json!({"plans": [{"code": "enterprise", "amount": 1}]})),
    )
    .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.text());
    assert_eq!(audit_for_action(&app, "admin.plans.updated").await.len(), 1);
    drop_plans_db(app, TAG).await;
}
