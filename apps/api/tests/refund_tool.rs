//! P1-108: the refund tool stub — live, through the real
//! HTTP layer.
//!
//! - `refund_tool_returns_checklist_guidance_and_audits` —
//!   the AC: an admin refund request returns the guided
//!   checklist (the Stripe dashboard deep-link + note, four
//!   steps all `done: false`, the required fields — values
//!   where known, `null` for `amount` /
//!   `invoice_or_payment_id`), writes exactly one
//!   `billing.refund.requested` audit row (actor the admin,
//!   reason the support note), changes NO state (the row
//!   stays `active`), and sends NO mail. A second call
//!   writes a second row — a note per request, not
//!   idempotent.
//! - `refund_tool_refunds_any_status_including_canceled` —
//!   a canceled subscription refunds too (200, status
//!   `canceled`); a row with no provider subscription id
//!   gets the dashboard subscriptions list URL, not a
//!   broken link.
//! - `refund_tool_denies_non_admin_callers` — anonymous
//!   401, plain user 403, support 403 (support stays
//!   read-only per FR-A-07), an admin user's own API key 403.
//! - `refund_tool_unknown_subscription_is_404` — no such
//!   row is 404 `PF-2010` and audits nothing.
//! - `refund_tool_rejects_bad_reason` — the audited note is
//!   required (the P1-008 suspend precedent): a missing,
//!   blank, or over-long `reason` is 422.

mod common;

use axum::http::{Method, Request, StatusCode};
use chrono::{Duration, Utc};
use common::{spawn_app, TestApp};
use detag_api::services::support_cancel::REFUND_GUIDANCE_NOTE;
use detag_core::repos::{audit_logs, email_outbox, plans, subscriptions};
use serde_json::json;
use uuid::Uuid;

/// The request id: a valid UUID, echoed by the middleware.
const REQUEST_ID: &str = "123e4567-e89b-42d3-a456-426614174112";

/// The password every test user registers with.
const PASSWORD: &str = "correct horse battery staple";

/// A per-request XFF address: each request gets its own IP
/// bucket, so the tests never trip each other's rate limits
/// (the P1-003 lesson).
fn unique_xff() -> String {
    format!("203.0.113.{}", Uuid::now_v7())
}

fn unique_email(prefix: &str) -> String {
    format!("{prefix}-{}@example.com", Uuid::now_v7())
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

/// Mint a full-scope key for the credential holder; return
/// the secret.
async fn create_key(app: &TestApp, token: &str) -> String {
    let body = json!({ "name": "refund-tool probe", "scopes": ["jobs:read", "jobs:write", "webhooks:manage"] });
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

/// Seed a subscription row directly — there is no billing
/// flow yet to create one through the API (P1-065 owns
/// creation). Returns the row id. `provider_subscription_id`
/// is `Some` for the dashboard-URL tests, `None` for the
/// no-provider-id test.
async fn seed_subscription(
    app: &TestApp,
    user_id: Uuid,
    plan_code: &str,
    status: &str,
    provider_subscription_id: Option<&str>,
) -> Uuid {
    let plan = plans::by_code(&app.pool, plan_code)
        .await
        .expect("load plan")
        .expect("plan is seeded");
    let row = sqlx::query(
        "INSERT INTO subscriptions (user_id, plan_id, provider, provider_customer_id, provider_subscription_id, status, current_period_end) \
         VALUES ($1, $2, 'stripe', $3, $4, $5, $6) RETURNING id",
    )
    .bind(user_id)
    .bind(plan.id)
    .bind(format!("cus_p108_{}", Uuid::now_v7()))
    .bind(provider_subscription_id)
    .bind(status)
    .bind(Utc::now() + Duration::days(30))
    .fetch_one(&app.pool)
    .await
    .expect("seed subscription");
    use sqlx::Row;
    row.try_get::<Uuid, _>("id").expect("seed returns id")
}

async fn row_of(app: &TestApp, id: Uuid) -> subscriptions::Subscription {
    subscriptions::by_id(&app.pool, id)
        .await
        .expect("load row")
        .expect("row exists")
}

async fn refund_rows(app: &TestApp, id: Uuid) -> Vec<audit_logs::AuditLog> {
    audit_logs::list_for_target(&app.pool, "subscription", &id.to_string())
        .await
        .expect("load audit")
        .into_iter()
        .filter(|row| row.action == "billing.refund.requested")
        .collect()
}

async fn mails_for_user(
    app: &TestApp,
    template: &str,
    user_id: Uuid,
) -> Vec<email_outbox::OutboxMail> {
    email_outbox::unsent_for_template(&app.pool, template)
        .await
        .expect("load outbox")
        .into_iter()
        .filter(|mail| {
            mail.payload.get("user_id").and_then(|v| v.as_str()) == Some(&user_id.to_string())
        })
        .collect()
}

/// POST the refund endpoint with an optional credential
/// (`None` = anonymous) and a JSON body.
async fn refund(
    app: &TestApp,
    subscription_id: Uuid,
    credential: Option<&str>,
    body: serde_json::Value,
) -> common::TestResponse {
    let mut req = Request::builder()
        .method(Method::POST)
        .uri(format!(
            "/v1/admin/billing/subscriptions/{subscription_id}/refund"
        ))
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff())
        .header("content-type", "application/json");
    if let Some(credential) = credential {
        req = req.header("authorization", format!("Bearer {credential}"));
    }
    app.request(
        req.body(axum::body::Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap(),
    )
    .await
}

fn problem_code(res: &common::TestResponse) -> String {
    res.json()["code"].as_str().unwrap_or("").to_string()
}

/// The `value` of one required field by name (`None` = the
/// field is absent; `Some(Value::Null)` = present and null).
fn required_field(body: &serde_json::Value, name: &str) -> Option<serde_json::Value> {
    body["required_fields"]
        .as_array()
        .unwrap_or_else(|| panic!("required_fields is an array: {body}"))
        .iter()
        .find(|f| f["field"].as_str() == Some(name))
        .map(|f| f["value"].clone())
}

#[tokio::test]
async fn refund_tool_returns_checklist_guidance_and_audits() {
    let app = spawn_app().await;
    let (admin_id, admin_token) = make_role(&app, &unique_email("admin"), "admin").await;
    let user = app.seed_user_unique().await;
    let provider_sub = format!("sub_p108_{}", Uuid::now_v7());
    let sub = seed_subscription(&app, user.id, "pro", "active", Some(&provider_sub)).await;
    let customer_id = row_of(&app, sub).await.provider_customer_id.unwrap();

    let res = refund(
        &app,
        sub,
        Some(&admin_token),
        json!({ "reason": "duplicate charge, refund in full" }),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let body = res.json();
    assert_eq!(body["subscription_id"].as_str().unwrap(), sub.to_string());
    assert_eq!(body["user_id"].as_str().unwrap(), user.id.to_string());
    assert_eq!(body["status"].as_str().unwrap(), "active");
    assert_eq!(body["plan_code"].as_str().unwrap(), "pro");
    // The guidance is a dashboard link only — no refund API
    // call, no money moved here.
    assert_eq!(
        body["refund_guidance"]["dashboard_url"].as_str().unwrap(),
        format!("https://dashboard.stripe.com/subscriptions/{provider_sub}")
    );
    assert_eq!(
        body["refund_guidance"]["note"].as_str().unwrap(),
        REFUND_GUIDANCE_NOTE,
        "the note is the P1-071 const verbatim"
    );
    // The checklist: four steps, all undone, in order.
    let checklist = body["checklist"].as_array().unwrap();
    assert_eq!(checklist.len(), 4, "the checklist is four steps");
    for (i, step) in checklist.iter().enumerate() {
        assert_eq!(step["step"].as_u64(), Some((i + 1) as u64));
        assert_eq!(step["done"].as_bool(), Some(false), "no step is done");
        assert!(
            !step["detail"].as_str().unwrap().is_empty(),
            "every step says what to do"
        );
    }
    // The required fields: values where known, null where
    // only support knows.
    assert_eq!(
        required_field(&body, "subscription_id"),
        Some(json!(sub.to_string()))
    );
    assert_eq!(
        required_field(&body, "provider_customer_id"),
        Some(json!(customer_id))
    );
    assert_eq!(
        required_field(&body, "reason"),
        Some(json!("duplicate charge, refund in full"))
    );
    assert_eq!(
        required_field(&body, "amount"),
        Some(serde_json::Value::Null),
        "amount is null — only support knows it"
    );
    assert_eq!(
        required_field(&body, "invoice_or_payment_id"),
        Some(serde_json::Value::Null),
        "invoice id is null — no provider read is made"
    );

    // NO state change: the row is untouched.
    let row = row_of(&app, sub).await;
    assert_eq!(row.status, "active");
    assert!(!row.cancel_at_period_end);

    // Exactly one audit row: the request, actor the admin,
    // reason the support note, after carries the link.
    let rows = refund_rows(&app, sub).await;
    assert_eq!(rows.len(), 1, "one audit row for the request");
    let row = &rows[0];
    assert_eq!(row.actor_user_id, Some(admin_id), "actor is the admin");
    assert_eq!(row.target_type, "subscription");
    assert_eq!(row.target_id.as_deref(), Some(sub.to_string()).as_deref());
    assert_eq!(
        row.reason.as_deref(),
        Some("duplicate charge, refund in full")
    );
    assert!(row.before.is_none(), "a request has no before state");
    assert_eq!(
        row.after.as_ref().unwrap()["subscription_id"],
        sub.to_string()
    );
    assert_eq!(
        row.after.as_ref().unwrap()["dashboard_url"],
        format!("https://dashboard.stripe.com/subscriptions/{provider_sub}")
    );

    // NO mail: the tool guides a human; it mails nobody.
    assert_eq!(
        mails_for_user(&app, "subscription-canceled", user.id)
            .await
            .len(),
        0,
        "no canceled mail"
    );

    // A second call is a second note, not a no-op: one more
    // row, same shape, the new reason verbatim.
    let res = refund(
        &app,
        sub,
        Some(&admin_token),
        json!({ "reason": "second request, partial refund" }),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let rows = refund_rows(&app, sub).await;
    assert_eq!(rows.len(), 2, "every call writes one row");
    let reasons: Vec<&str> = rows.iter().map(|r| r.reason.as_deref().unwrap()).collect();
    assert!(reasons.contains(&"duplicate charge, refund in full"));
    assert!(reasons.contains(&"second request, partial refund"));
    assert_eq!(row_of(&app, sub).await.status, "active");
}

#[tokio::test]
async fn refund_tool_refunds_any_status_including_canceled() {
    let app = spawn_app().await;
    let (_admin_id, admin_token) = make_role(&app, &unique_email("admin"), "admin").await;
    let user = app.seed_user_unique().await;
    // Already-canceled, and no provider subscription id: the
    // common case (cancel first, refund after) with the list
    // URL as the fallback.
    let sub = seed_subscription(&app, user.id, "pro", "canceled", None).await;

    let res = refund(
        &app,
        sub,
        Some(&admin_token),
        json!({ "reason": "canceled last week, refund the remainder" }),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let body = res.json();
    assert_eq!(body["status"].as_str().unwrap(), "canceled");
    assert_eq!(
        body["refund_guidance"]["dashboard_url"].as_str().unwrap(),
        "https://dashboard.stripe.com/subscriptions",
        "no provider id means the list page, not a broken link"
    );
    assert_eq!(refund_rows(&app, sub).await.len(), 1);
    assert_eq!(row_of(&app, sub).await.status, "canceled");
}

#[tokio::test]
async fn refund_tool_denies_non_admin_callers() {
    let app = spawn_app().await;
    let (admin_id, admin_token) = make_role(&app, &unique_email("admin"), "admin").await;
    let (_support_id, support_token) = make_role(&app, &unique_email("support"), "support").await;
    let (_user_id, user_token) = make_role(&app, &unique_email("user"), "user").await;
    // P1-055: key creation is plan-gated — the admin user
    // needs a plan first.
    app.grant_plan(admin_id, "pro").await;
    let admin_key = create_key(&app, &admin_token).await;
    let user = app.seed_user_unique().await;
    let sub = seed_subscription(&app, user.id, "pro", "active", None).await;
    let body = json!({ "reason": "denied probe" });

    // Anonymous: 401 PF-1001.
    let res = refund(&app, sub, None, body.clone()).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-1001");
    // Plain user: 403 PF-1002.
    let res = refund(&app, sub, Some(&user_token), body.clone()).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-1002");
    // Support: 403 — support stays read-only (FR-A-07).
    let res = refund(&app, sub, Some(&support_token), body.clone()).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-1002");
    // An admin user's own API key: 403 — no admin ever.
    let res = refund(&app, sub, Some(&admin_key), body.clone()).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-1002");

    // None of the denials changed anything or audited.
    assert_eq!(row_of(&app, sub).await.status, "active");
    assert_eq!(refund_rows(&app, sub).await.len(), 0);
}

#[tokio::test]
async fn refund_tool_unknown_subscription_is_404() {
    let app = spawn_app().await;
    let (_admin_id, admin_token) = make_role(&app, &unique_email("admin"), "admin").await;
    let unknown = Uuid::now_v7();

    let res = refund(
        &app,
        unknown,
        Some(&admin_token),
        json!({ "reason": "no such subscription" }),
    )
    .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-2010");
    assert_eq!(refund_rows(&app, unknown).await.len(), 0);
}

#[tokio::test]
async fn refund_tool_rejects_bad_reason() {
    let app = spawn_app().await;
    let (_admin_id, admin_token) = make_role(&app, &unique_email("admin"), "admin").await;
    let user = app.seed_user_unique().await;
    let sub = seed_subscription(&app, user.id, "pro", "active", None).await;

    // Missing reason.
    let res = refund(&app, sub, Some(&admin_token), json!({})).await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    assert_eq!(problem_code(&res), "PF-2002");
    // Blank reason.
    let res = refund(&app, sub, Some(&admin_token), json!({ "reason": "   " })).await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    assert_eq!(problem_code(&res), "PF-2002");
    // Over-long reason (501 chars).
    let res = refund(
        &app,
        sub,
        Some(&admin_token),
        json!({ "reason": "x".repeat(501) }),
    )
    .await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    assert_eq!(problem_code(&res), "PF-2002");

    // None of the rejections audited.
    assert_eq!(refund_rows(&app, sub).await.len(), 0);
}
