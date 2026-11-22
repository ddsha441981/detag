//! P1-071: the support-facing refund/cancel policy endpoint —
//! live, through the real HTTP layer.
//!
//! - `support_cancel_cancels_immediately_and_audits` — the
//!   AC: an admin cancel moves the row to `canceled` NOW
//!   (not at period end like a P1-069 downgrade), with the
//!   policy snippet + the Stripe-dashboard refund guidance
//!   in the response, exactly one
//!   `billing.subscription.status_changed` audit row (actor
//!   the admin, client `"admin"`, reason the support note),
//!   and the `subscription-canceled` mail.
//! - `support_cancel_second_call_is_a_noop` — canceling an
//!   already-canceled subscription is a 200 no-op
//!   (`changed: false`): no second audit row, no second mail.
//! - `support_cancel_denies_non_admin_callers` — anonymous
//!   401, plain user 403, support 403 (support stays
//!   read-only per FR-A-07), an admin user's own API key 403.
//! - `support_cancel_unknown_subscription_is_404` — no such
//!   row is 404 `PF-2010` and audits nothing.
//! - `support_cancel_rejects_blank_reason` — the audited
//!   note is required (the P1-008 suspend precedent): a
//!   missing or blank `reason` is 422.
//! - `support_cancel_clears_dunning_state` — canceling a
//!   `past_due` row clears `past_due_since` / `dunning_step`
//!   (the P1-070 CHECK: a canceled row carries no dunning
//!   state).
//! - `support_cancel_moots_pending_scheduled_change` — a
//!   pending P1-069 downgrade for the subscription closes
//!   as `canceled` when support cancels now, so the period
//!   roll can never apply it to a canceled row.
//! - `support_cancel_dashboard_url_without_provider_id` —
//!   a row with no provider subscription id gets the
//!   dashboard subscriptions list URL, not a broken link.

mod common;

use axum::http::{Method, Request, StatusCode};
use chrono::{Duration, Utc};
use common::{spawn_app, TestApp};
use detag_core::repos::{audit_logs, email_outbox, plan_changes, plans, subscriptions};
use serde_json::json;
use uuid::Uuid;

/// The request id: a valid UUID, echoed by the middleware.
const REQUEST_ID: &str = "123e4567-e89b-42d3-a456-426614174111";

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
    let body = json!({ "name": "support-cancel probe", "scopes": ["jobs:read", "jobs:write", "webhooks:manage"] });
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
    let (past_due_since, dunning_step) = if status == "past_due" {
        (Some(Utc::now()), 3)
    } else {
        (None, 0)
    };
    let row = sqlx::query(
        "INSERT INTO subscriptions (user_id, plan_id, provider, provider_customer_id, provider_subscription_id, status, current_period_end, past_due_since, dunning_step) \
         VALUES ($1, $2, 'stripe', $3, $4, $5, $6, $7, $8) RETURNING id",
    )
    .bind(user_id)
    .bind(plan.id)
    .bind(format!("cus_p1071_{}", Uuid::now_v7()))
    .bind(provider_subscription_id)
    .bind(status)
    .bind(Utc::now() + Duration::days(30))
    .bind(past_due_since)
    .bind(dunning_step)
    .fetch_one(&app.pool)
    .await
    .expect("seed subscription");
    use sqlx::Row;
    row.try_get::<Uuid, _>("id").expect("seed returns id")
}

/// Seed a pending scheduled plan change (P1-069) for the
/// subscription: a downgrade to Free, effective at the
/// period end.
async fn seed_scheduled_change(
    app: &TestApp,
    user_id: Uuid,
    subscription_id: Uuid,
    from_plan_code: &str,
    to_plan_code: &str,
) -> Uuid {
    let from = plans::by_code(&app.pool, from_plan_code)
        .await
        .expect("load from plan")
        .expect("from plan is seeded");
    let to = plans::by_code(&app.pool, to_plan_code)
        .await
        .expect("load to plan")
        .expect("to plan is seeded");
    let change = plan_changes::insert(
        &app.pool,
        user_id,
        subscription_id,
        from.id,
        to.id,
        Utc::now() + Duration::days(30),
    )
    .await
    .expect("seed scheduled change");
    change.id
}

async fn row_of(app: &TestApp, id: Uuid) -> subscriptions::Subscription {
    subscriptions::by_id(&app.pool, id)
        .await
        .expect("load row")
        .expect("row exists")
}

async fn audit_rows(app: &TestApp, id: Uuid) -> Vec<audit_logs::AuditLog> {
    audit_logs::list_for_target(&app.pool, "subscription", &id.to_string())
        .await
        .expect("load audit")
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

/// POST the cancel endpoint with an optional credential
/// (`None` = anonymous) and a JSON body.
async fn cancel(
    app: &TestApp,
    subscription_id: Uuid,
    credential: Option<&str>,
    body: serde_json::Value,
) -> common::TestResponse {
    let mut req = Request::builder()
        .method(Method::POST)
        .uri(format!(
            "/v1/admin/billing/subscriptions/{subscription_id}/cancel"
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

#[tokio::test]
async fn support_cancel_cancels_immediately_and_audits() {
    let app = spawn_app().await;
    let (admin_id, admin_token) = make_role(&app, &unique_email("admin"), "admin").await;
    let user = app.seed_user_unique().await;
    let provider_sub = format!("sub_p1071_{}", Uuid::now_v7());
    let sub = seed_subscription(&app, user.id, "pro", "active", Some(&provider_sub)).await;

    let res = cancel(
        &app,
        sub,
        Some(&admin_token),
        json!({ "reason": "customer asked to cancel" }),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let body = res.json();
    assert_eq!(body["subscription_id"].as_str().unwrap(), sub.to_string());
    assert_eq!(body["user_id"].as_str().unwrap(), user.id.to_string());
    assert_eq!(body["status"].as_str().unwrap(), "canceled");
    assert_eq!(body["plan_code"].as_str().unwrap(), "pro");
    assert_eq!(body["changed"].as_bool(), Some(true));
    // The ToS snippet rides the response.
    assert!(
        body["policy"]
            .as_str()
            .unwrap()
            .contains("No prorated refund is issued automatically"),
        "policy snippet: {}",
        body["policy"]
    );
    // The refund guidance is a dashboard link only — no
    // refund API call, no money moved here.
    assert_eq!(
        body["refund_guidance"]["dashboard_url"].as_str().unwrap(),
        format!("https://dashboard.stripe.com/subscriptions/{provider_sub}")
    );
    assert!(
        body["refund_guidance"]["note"]
            .as_str()
            .unwrap()
            .contains("Stripe dashboard"),
        "guidance note: {}",
        body["refund_guidance"]["note"]
    );

    // The row moved NOW — not at period end (P1-069).
    let row = row_of(&app, sub).await;
    assert_eq!(row.status, "canceled");
    assert!(!row.cancel_at_period_end);
    assert!(
        row.current_period_end.is_some(),
        "period end kept as history"
    );

    // Exactly one audit row: the status change, actor the
    // admin, client "admin", reason the support note.
    let rows = audit_rows(&app, sub).await;
    assert_eq!(rows.len(), 1, "one audit row for the cancel");
    let row = &rows[0];
    assert_eq!(row.action, "billing.subscription.status_changed");
    assert_eq!(row.actor_user_id, Some(admin_id), "actor is the admin");
    assert_eq!(row.actor_client.as_deref(), Some("admin"));
    assert_eq!(row.target_type, "subscription");
    assert_eq!(row.target_id.as_deref(), Some(sub.to_string()).as_deref());
    assert_eq!(row.reason.as_deref(), Some("customer asked to cancel"));
    assert_eq!(row.before.as_ref().unwrap()["status"], "active");
    assert_eq!(row.after.as_ref().unwrap()["status"], "canceled");
    assert_eq!(row.after.as_ref().unwrap()["plan_code"], "pro");

    // The customer gets the canceled mail (the P1-064
    // lifecycle mail for any move to `canceled`).
    assert_eq!(
        mails_for_user(&app, "subscription-canceled", user.id)
            .await
            .len(),
        1,
        "one canceled mail"
    );
}

#[tokio::test]
async fn support_cancel_second_call_is_a_noop() {
    let app = spawn_app().await;
    let (_admin_id, admin_token) = make_role(&app, &unique_email("admin"), "admin").await;
    let user = app.seed_user_unique().await;
    let sub = seed_subscription(&app, user.id, "pro", "active", None).await;

    let first = cancel(
        &app,
        sub,
        Some(&admin_token),
        json!({ "reason": "first cancel" }),
    )
    .await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.text());
    assert_eq!(first.json()["changed"].as_bool(), Some(true));

    let second = cancel(
        &app,
        sub,
        Some(&admin_token),
        json!({ "reason": "second cancel" }),
    )
    .await;
    assert_eq!(second.status, StatusCode::OK, "{}", second.text());
    assert_eq!(second.json()["changed"].as_bool(), Some(false));
    assert_eq!(second.json()["status"].as_str().unwrap(), "canceled");

    // No second audit row, no second mail.
    assert_eq!(audit_rows(&app, sub).await.len(), 1);
    assert_eq!(
        mails_for_user(&app, "subscription-canceled", user.id)
            .await
            .len(),
        1
    );
}

#[tokio::test]
async fn support_cancel_denies_non_admin_callers() {
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
    let res = cancel(&app, sub, None, body.clone()).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-1001");
    // Plain user: 403 PF-1002.
    let res = cancel(&app, sub, Some(&user_token), body.clone()).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-1002");
    // Support: 403 — support stays read-only (FR-A-07).
    let res = cancel(&app, sub, Some(&support_token), body.clone()).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-1002");
    // An admin user's own API key: 403 — no admin ever.
    let res = cancel(&app, sub, Some(&admin_key), body.clone()).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-1002");

    // None of the denials changed anything or audited.
    assert_eq!(row_of(&app, sub).await.status, "active");
    assert_eq!(audit_rows(&app, sub).await.len(), 0);
}

#[tokio::test]
async fn support_cancel_unknown_subscription_is_404() {
    let app = spawn_app().await;
    let (_admin_id, admin_token) = make_role(&app, &unique_email("admin"), "admin").await;
    let unknown = Uuid::now_v7();

    let res = cancel(
        &app,
        unknown,
        Some(&admin_token),
        json!({ "reason": "no such subscription" }),
    )
    .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-2010");
    assert_eq!(audit_rows(&app, unknown).await.len(), 0);
}

#[tokio::test]
async fn support_cancel_rejects_blank_reason() {
    let app = spawn_app().await;
    let (_admin_id, admin_token) = make_role(&app, &unique_email("admin"), "admin").await;
    let user = app.seed_user_unique().await;
    let sub = seed_subscription(&app, user.id, "pro", "active", None).await;

    // Missing reason.
    let res = cancel(&app, sub, Some(&admin_token), json!({})).await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    // Blank reason (whitespace only).
    let res = cancel(&app, sub, Some(&admin_token), json!({ "reason": "   " })).await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );

    // Nothing changed, nothing audited.
    assert_eq!(row_of(&app, sub).await.status, "active");
    assert_eq!(audit_rows(&app, sub).await.len(), 0);
}

#[tokio::test]
async fn support_cancel_clears_dunning_state() {
    let app = spawn_app().await;
    let (_admin_id, admin_token) = make_role(&app, &unique_email("admin"), "admin").await;
    let user = app.seed_user_unique().await;
    let sub = seed_subscription(&app, user.id, "pro", "past_due", None).await;
    assert!(row_of(&app, sub).await.past_due_since.is_some());
    assert_eq!(row_of(&app, sub).await.dunning_step, 3);

    let res = cancel(
        &app,
        sub,
        Some(&admin_token),
        json!({ "reason": "dunning cancel" }),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());

    // A canceled row carries no dunning state (P1-070
    // CHECK) — the move out of `past_due` cleared both.
    let row = row_of(&app, sub).await;
    assert_eq!(row.status, "canceled");
    assert!(row.past_due_since.is_none());
    assert_eq!(row.dunning_step, 0);
}

#[tokio::test]
async fn support_cancel_clears_cancel_at_period_end() {
    let app = spawn_app().await;
    let (_admin_id, admin_token) = make_role(&app, &unique_email("admin"), "admin").await;
    let user = app.seed_user_unique().await;
    let sub = seed_subscription(&app, user.id, "pro", "active", None).await;
    // The row was scheduled to cancel at period end (a
    // provider-side schedule, the P1-065 shape) — the
    // immediate cancel clears the flag: a canceled row has
    // no pending period-end cancel.
    sqlx::query("UPDATE subscriptions SET cancel_at_period_end = true WHERE id = $1")
        .bind(sub)
        .execute(&app.pool)
        .await
        .expect("set cancel_at_period_end");
    assert!(row_of(&app, sub).await.cancel_at_period_end);

    let res = cancel(
        &app,
        sub,
        Some(&admin_token),
        json!({ "reason": "cancel now, not at period end" }),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());

    let row = row_of(&app, sub).await;
    assert_eq!(row.status, "canceled");
    assert!(!row.cancel_at_period_end);
}

#[tokio::test]
async fn support_cancel_moots_pending_scheduled_change() {
    let app = spawn_app().await;
    let (_admin_id, admin_token) = make_role(&app, &unique_email("admin"), "admin").await;
    let user = app.seed_user_unique().await;
    let sub = seed_subscription(&app, user.id, "pro", "active", None).await;
    let change = seed_scheduled_change(&app, user.id, sub, "pro", "free").await;

    let res = cancel(
        &app,
        sub,
        Some(&admin_token),
        json!({ "reason": "cancel beats the scheduled downgrade" }),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());

    // The pending downgrade closed as `canceled` — the
    // period roll can never apply it to a canceled row.
    let closed = plan_changes::by_id(&app.pool, change)
        .await
        .expect("load change")
        .expect("change exists");
    assert_eq!(closed.status, "canceled");
}

#[tokio::test]
async fn support_cancel_dashboard_url_without_provider_id() {
    let app = spawn_app().await;
    let (_admin_id, admin_token) = make_role(&app, &unique_email("admin"), "admin").await;
    let user = app.seed_user_unique().await;
    let sub = seed_subscription(&app, user.id, "pro", "active", None).await;

    let res = cancel(
        &app,
        sub,
        Some(&admin_token),
        json!({ "reason": "no provider id on file" }),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(
        res.json()["refund_guidance"]["dashboard_url"]
            .as_str()
            .unwrap(),
        "https://dashboard.stripe.com/subscriptions"
    );
}
