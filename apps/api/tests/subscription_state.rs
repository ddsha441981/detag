//! P1-064: the subscription state machine — service side, live.
//!
//! The pure rules are unit-tested in `detag-billing`
//! (`state_machine::transition_matrix` covers all 16 pairs);
//! these tests prove the service that P1-065 will call:
//! the guarded row write, the audit row, the lifecycle mail,
//! and the entitlement change the next resolution sees.
//!
//! - `trialing_to_active_welcomes_and_grants` — the trial
//!   converts: row `active`, one `subscription-welcome` outbox
//!   row (payload `{user_id, plan_code: "pro"}`), one audit
//!   row, and `effective_plan` resolves Pro.
//! - `active_to_past_due_duns_and_revokes` — a failed payment:
//!   row `past_due`, one `subscription-dunning` mail, and the
//!   user falls back to Free (P1-049 grants nothing past due).
//! - `past_due_to_active_recovers_silently` — recovery mails
//!   nothing and grants Pro again.
//! - `invalid_transition_rejected_and_unchanged` — the AC:
//!   `canceled → active` is 409 `PF-5002`, the row is
//!   untouched, and no audit row or mail is written.
//! - `same_state_is_a_noop` — replaying the current state
//!   writes nothing at all (single effect, P1-066).
//! - `domain_event_deleted_cancels_and_invoice_paid_is_noop`
//!   — the P1-065 entry point: a delete cancels (with the
//!   canceled mail); an invoice payment moves nothing.
//! - `unknown_row_is_not_found` — an unknown row is 404
//!   `PF-2010`, never an invented row.

mod common;

use common::{spawn_app, TestApp};
use detag_api::services::{
    entitlements::effective_plan,
    subscription_state::{apply_domain_event, apply_transition},
};
use detag_billing::{DomainEvent, SubscriptionStatus};
use detag_core::{
    error::AppError,
    repos::{audit_logs, email_outbox, plans, subscriptions},
};
use sqlx::Row;
use uuid::Uuid;

/// Insert a subscription row directly — there is no billing
/// flow yet to create one through the API (P1-065 owns
/// creation). Returns the row id.
async fn seed_subscription(app: &TestApp, user_id: Uuid, plan_code: &str, status: &str) -> Uuid {
    let plan = plans::by_code(&app.pool, plan_code)
        .await
        .expect("load plan")
        .expect("plan is seeded");
    // Plain `sqlx::query` (not `query!`): the seed needs no
    // offline metadata, like the plans suite's seeder.
    let row = sqlx::query(
        "INSERT INTO subscriptions (user_id, plan_id, provider, provider_subscription_id, status) \
         VALUES ($1, $2, 'stripe', 'sub_test_1', $3) RETURNING id",
    )
    .bind(user_id)
    .bind(plan.id)
    .bind(status)
    .fetch_one(&app.pool)
    .await
    .expect("seed subscription");
    row.try_get::<Uuid, _>("id").expect("seed returns id")
}

async fn status_of(app: &TestApp, id: Uuid) -> String {
    subscriptions::by_id(&app.pool, id)
        .await
        .expect("load row")
        .expect("row exists")
        .status
}

async fn audit_rows(app: &TestApp, id: Uuid) -> Vec<detag_core::repos::audit_logs::AuditLog> {
    audit_logs::list_for_target(&app.pool, "subscription", &id.to_string())
        .await
        .expect("load audit")
}

async fn mails_for(
    app: &TestApp,
    template: &str,
) -> Vec<detag_core::repos::email_outbox::OutboxMail> {
    email_outbox::unsent_for_template(&app.pool, template)
        .await
        .expect("load outbox")
}

/// The mails for one user only: the harness DB is shared by
/// every test in this binary (the P1-046 precedent), so a
/// global count would count other tests' mails too.
async fn mails_for_user(
    app: &TestApp,
    template: &str,
    user_id: Uuid,
) -> Vec<detag_core::repos::email_outbox::OutboxMail> {
    mails_for(app, template)
        .await
        .into_iter()
        .filter(|mail| {
            mail.payload.get("user_id").and_then(|v| v.as_str()) == Some(&user_id.to_string())
        })
        .collect()
}

async fn plan_code_of(app: &TestApp, user_id: Uuid) -> String {
    effective_plan(&app.state.plans, &app.pool, user_id)
        .await
        .expect("resolve plan")
        .plan
        .code
}

#[tokio::test]
async fn trialing_to_active_welcomes_and_grants() {
    let app = spawn_app().await;
    let user = app.seed_user_unique().await;
    let sub = seed_subscription(&app, user.id, "pro", "trialing").await;

    let applied = apply_transition(
        &app.state,
        sub,
        SubscriptionStatus::Active,
        "evt_test_welcome",
    )
    .await
    .expect("trialing -> active applies");
    assert!(applied.changed);
    assert_eq!(applied.mail_template, Some("subscription-welcome"));
    assert_eq!(status_of(&app, sub).await, "active");
    assert_eq!(plan_code_of(&app, user.id).await, "pro");

    let audit = audit_rows(&app, sub).await;
    assert_eq!(audit.len(), 1, "exactly one audit row");
    assert_eq!(audit[0].action, "billing.subscription.status_changed");
    assert_eq!(
        audit[0].before,
        Some(serde_json::json!({ "status": "trialing" }))
    );
    assert_eq!(
        audit[0].after,
        Some(serde_json::json!({ "status": "active", "plan_code": "pro" }))
    );
    assert_eq!(audit[0].reason.as_deref(), Some("evt_test_welcome"));

    let mails = mails_for_user(&app, "subscription-welcome", user.id).await;
    assert_eq!(mails.len(), 1, "exactly one welcome mail");
    assert_eq!(
        mails[0].payload,
        serde_json::json!({ "user_id": user.id, "plan_code": "pro" })
    );
}

#[tokio::test]
async fn active_to_past_due_duns_and_revokes() {
    let app = spawn_app().await;
    let user = app.seed_user_unique().await;
    let sub = seed_subscription(&app, user.id, "pro", "active").await;

    let applied = apply_transition(
        &app.state,
        sub,
        SubscriptionStatus::PastDue,
        "evt_test_dunning",
    )
    .await
    .expect("active -> past_due applies");
    assert!(applied.changed);
    assert_eq!(applied.mail_template, Some("subscription-dunning"));
    assert_eq!(status_of(&app, sub).await, "past_due");
    assert_eq!(
        plan_code_of(&app, user.id).await,
        "free",
        "past due grants nothing (P1-049)"
    );

    let mails = mails_for_user(&app, "subscription-dunning", user.id).await;
    assert_eq!(mails.len(), 1, "exactly one dunning mail");
    assert_eq!(audit_rows(&app, sub).await.len(), 1);
}

#[tokio::test]
async fn past_due_to_active_recovers_silently() {
    let app = spawn_app().await;
    let user = app.seed_user_unique().await;
    let sub = seed_subscription(&app, user.id, "pro", "past_due").await;

    let applied = apply_transition(
        &app.state,
        sub,
        SubscriptionStatus::Active,
        "evt_test_recovery",
    )
    .await
    .expect("past_due -> active applies");
    assert!(applied.changed);
    assert_eq!(applied.mail_template, None, "recovery mails nothing");
    assert_eq!(status_of(&app, sub).await, "active");
    assert_eq!(plan_code_of(&app, user.id).await, "pro");

    for template in [
        "subscription-welcome",
        "subscription-dunning",
        "subscription-canceled",
    ] {
        assert!(
            mails_for_user(&app, template, user.id).await.is_empty(),
            "no {template} mail on recovery"
        );
    }
    assert_eq!(audit_rows(&app, sub).await.len(), 1);
}

#[tokio::test]
async fn invalid_transition_rejected_and_unchanged() {
    let app = spawn_app().await;
    let user = app.seed_user_unique().await;
    let sub = seed_subscription(&app, user.id, "pro", "canceled").await;

    let err = apply_transition(&app.state, sub, SubscriptionStatus::Active, "evt_test_bad")
        .await
        .expect_err("canceled -> active must be rejected");
    assert!(matches!(err, AppError::InvalidStateTransition));
    assert_eq!(err.code(), "PF-5002");
    assert_eq!(status_of(&app, sub).await, "canceled", "row untouched");
    assert!(
        audit_rows(&app, sub).await.is_empty(),
        "a rejected move audits nothing"
    );
    for template in [
        "subscription-welcome",
        "subscription-dunning",
        "subscription-canceled",
    ] {
        assert!(
            mails_for_user(&app, template, user.id).await.is_empty(),
            "a rejected move mails nothing"
        );
    }
    // And the user stays Free: a canceled row never granted.
    assert_eq!(plan_code_of(&app, user.id).await, "free");
}

#[tokio::test]
async fn same_state_is_a_noop() {
    let app = spawn_app().await;
    let user = app.seed_user_unique().await;
    let sub = seed_subscription(&app, user.id, "pro", "active").await;

    let applied = apply_transition(
        &app.state,
        sub,
        SubscriptionStatus::Active,
        "evt_test_replay",
    )
    .await
    .expect("same-state applies as a no-op");
    assert!(!applied.changed);
    assert_eq!(applied.mail_template, None);
    assert_eq!(status_of(&app, sub).await, "active");
    assert!(
        audit_rows(&app, sub).await.is_empty(),
        "a no-op audits nothing"
    );
    assert!(
        mails_for_user(&app, "subscription-welcome", user.id)
            .await
            .is_empty(),
        "a no-op mails nothing"
    );
}

#[tokio::test]
async fn domain_event_deleted_cancels_and_invoice_paid_is_noop() {
    let app = spawn_app().await;
    let user = app.seed_user_unique().await;
    let sub = seed_subscription(&app, user.id, "pro", "active").await;

    let paid = apply_domain_event(
        &app.state,
        sub,
        &DomainEvent::InvoicePaid {
            invoice_id: "in_test_1".to_string(),
            subscription_id: Some("sub_test_1".to_string()),
        },
        "evt_test_invoice",
    )
    .await
    .expect("an invoice payment is not a transition");
    assert!(!paid.changed);
    assert_eq!(status_of(&app, sub).await, "active");

    let deleted = apply_domain_event(
        &app.state,
        sub,
        &DomainEvent::SubscriptionDeleted {
            subscription_id: "sub_test_1".to_string(),
        },
        "evt_test_deleted",
    )
    .await
    .expect("a delete cancels");
    assert!(deleted.changed);
    assert_eq!(deleted.to, SubscriptionStatus::Canceled);
    assert_eq!(deleted.mail_template, Some("subscription-canceled"));
    assert_eq!(status_of(&app, sub).await, "canceled");
    assert_eq!(plan_code_of(&app, user.id).await, "free");
    assert_eq!(
        mails_for_user(&app, "subscription-canceled", user.id)
            .await
            .len(),
        1
    );
    assert_eq!(audit_rows(&app, sub).await.len(), 1);
}

#[tokio::test]
async fn unknown_row_is_not_found() {
    let app = spawn_app().await;
    let err = apply_transition(
        &app.state,
        // A fixed UUID no seed ever mints (seeds use gen_random_uuid).
        Uuid::parse_str("00000000-0000-0000-0000-000000000099").unwrap(),
        SubscriptionStatus::Active,
        "evt_test_missing",
    )
    .await
    .expect_err("an unknown row must be 404");
    assert!(matches!(err, AppError::NotFound));
    assert_eq!(err.code(), "PF-2010");
}
