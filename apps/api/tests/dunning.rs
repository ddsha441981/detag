//! P1-070: the dunning policy job — live, on a frozen clock.
//!
//! The pure day/step matrix is unit-tested in
//! `services/dunning.rs` (`action_matrix`); these tests prove
//! the job itself: the sequence simulated via test-clock (the
//! AC), the exactly-once step guard, recovery mid-sequence,
//! the config kill-switch, and custom days.
//!
//! - `day_zero_is_the_past_due_mail` — the move to `past_due`
//!   IS day 0 (one `subscription-dunning` mail, the dunning
//!   clock stamped, step 0); a job pass on day 0 sends
//!   nothing more.
//! - `sequence_day3_day7_suspend_via_test_clock` — the AC:
//!   day-3 mail at +3d, nothing at +6d, day-7 mail at +7d,
//!   cancel at +14d (row `canceled`, canceled mail, the
//!   status_changed audit with reason `dunning-day-14`),
//!   and a pass after that finds no past_due row left.
//! - `rerun_same_day_sends_nothing_twice` — the step guard:
//!   two passes on day 3 mail exactly one day-3 reminder.
//! - `recovery_mid_sequence_stops_the_reminders` — a payment
//!   that heals at day 5 clears the dunning state and the
//!   day-7 mail never fires, even past day 14.
//! - `disabled_config_is_a_noop` — `dunning_enabled=false`
//!   pauses the job: no mails, no cancel, the row stays
//!   past due.
//! - `custom_days_follow_config` — day3=1 / day7=2 /
//!   suspend=5 move the whole sequence (the templates fire
//!   on configured days, not the literals 3/7/14).
//!
//! # Why the clock starts 14 days in the past
//!
//! The job is a GLOBAL scan — it acts on every `past_due`
//! row in the shared harness DB, not just this test's row.
//! The frozen clock therefore starts at real-now minus 14
//! days and only ever advances back up to real-now: any
//! other test's row (stamped at real-now by its own clock)
//! is 0 or fewer days past due from this job's point of
//! view, so the job always skips it. A clock that advanced
//! PAST real-now would suspend other tests' rows (the
//! FLAKY-006 lesson: the shared DB is not an isolation
//! boundary). The tests in this file also serialize on
//! [`DUNNING_TEST_LOCK`] and clean up their own rows, so a
//! sibling test's backdated row is never in the scan while
//! this test runs — and a stale row left by a failed run is
//! deleted at the next test's start (past_due for over a
//! day can only be such a leftover: no live test stamps a
//! row that old).
//!
//! # What the report counts prove
//!
//! `scanned` is only ever `>= 1`: other binaries seed
//! `past_due` rows concurrently, and they are counted (then
//! skipped). The action counts (`reminded_day3` /
//! `reminded_day7` / `suspended`) ARE exact — no other row
//! can be actionable from this job's clock — and the real
//! proof is per-user anyway: exactly one mail of each kind
//! for this test's user, the step on this test's row, and
//! the audit rows for this test's subscription.

mod common;

use std::sync::Arc;

use chrono::{Duration, Utc};
use common::{TestApp, TestFixtures};
use detag_api::services::{
    dunning::{run_dunning, DUNNING_REMINDER_ACTION},
    entitlements::effective_plan,
    subscription_state::apply_transition,
};
use detag_billing::SubscriptionStatus;
use detag_core::{
    clock::{Clock, ManualClock},
    config::Settings,
    repos::{audit_logs, email_outbox, plans, subscriptions},
};
use sqlx::Row;
use uuid::Uuid;

/// Serializes this file's tests: the job scans the shared
/// DB, so two of these tests must never run at once (see
/// the module docs). Held for the whole test body. A
/// `tokio` mutex, not a `std` one — a `std::sync::MutexGuard`
/// held across `.await` trips `clippy::await_holding_lock`
/// (`just verify` runs clippy with `-D warnings`).
static DUNNING_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The app on a frozen clock 14 days in the past (see the
/// module docs), with the fixtures' settings (the scratch
/// DB — never `Settings::default()`, whose database URL is
/// the dev database).
async fn dunning_app(clock: Arc<ManualClock>) -> (TestApp, Arc<ManualClock>) {
    dunning_app_with(clock, |_| {}).await
}

/// The app on a frozen clock 14 days in the past, with the
/// fixtures' settings patched by the caller (custom dunning
/// days, the kill-switch).
async fn dunning_app_with(
    clock: Arc<ManualClock>,
    patch: impl FnOnce(&mut Settings),
) -> (TestApp, Arc<ManualClock>) {
    let fixtures = TestFixtures::from_env().await;
    let mut settings = fixtures.settings();
    patch(&mut settings);
    let clock_dyn: Arc<dyn Clock> = Arc::clone(&clock) as Arc<dyn Clock>;
    let app = TestApp::spawn_with_fixtures_settings_and_clock(fixtures, settings, clock_dyn).await;
    (app, clock)
}

/// A frozen clock 14 days in the past (see the module docs).
fn test_clock() -> Arc<ManualClock> {
    Arc::new(ManualClock::new(Utc::now() - Duration::days(14)))
}

/// Delete stale `past_due` rows left by a failed run: a row
/// past due for over a day can only be such a leftover (no
/// live test stamps a row that old — other tests stamp
/// real-now). Run at test start, under the lock, so no
/// sibling test's row can be live.
async fn cleanup_stale_past_due(app: &TestApp) {
    sqlx::query(
        "DELETE FROM subscriptions \
         WHERE status = 'past_due' AND past_due_since < now() - INTERVAL '1 day'",
    )
    .execute(&app.pool)
    .await
    .expect("cleanup stale past_due rows");
}

/// Delete this test's row at the end, so the next run's
/// report counts stay exact (see the module docs).
async fn cleanup_row(app: &TestApp, id: Uuid) {
    sqlx::query("DELETE FROM subscriptions WHERE id = $1")
        .bind(id)
        .execute(&app.pool)
        .await
        .expect("cleanup test row");
}

/// Insert a subscription row directly — there is no billing
/// flow yet to create one through the API (P1-065 owns
/// creation). Returns the row id.
async fn seed_subscription(app: &TestApp, user_id: Uuid, plan_code: &str, status: &str) -> Uuid {
    let plan = plans::by_code(&app.pool, plan_code)
        .await
        .expect("load plan")
        .expect("plan is seeded");
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

/// Drive a subscription into `past_due` through the real
/// move (day 0): the day-0 mail lands and the dunning clock
/// is stamped from the frozen clock.
async fn fail_payment(app: &TestApp, sub: Uuid) {
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
}

#[tokio::test]
async fn day_zero_is_the_past_due_mail() {
    let _guard = DUNNING_TEST_LOCK.lock().await;
    let clock = test_clock();
    let (app, _clock) = dunning_app(clock).await;
    cleanup_stale_past_due(&app).await;
    let user = app.seed_user_unique().await;
    let sub = seed_subscription(&app, user.id, "pro", "active").await;

    fail_payment(&app, sub).await;
    assert_eq!(row_of(&app, sub).await.status, "past_due");

    // Exactly one day-0 mail — and the job adds none on day 0.
    assert_eq!(
        mails_for_user(&app, "subscription-dunning", user.id)
            .await
            .len(),
        1,
        "day 0 is the move's mail, and only that mail"
    );
    let report = run_dunning(&app.state).await.expect("job runs");
    assert!(report.ran);
    assert!(report.scanned >= 1, "this row is scanned");
    assert_eq!(report.reminded_day3, 0);
    assert_eq!(report.reminded_day7, 0);
    assert_eq!(report.suspended, 0);
    assert_eq!(report.errors, 0);
    for template in [
        "subscription-dunning-day3",
        "subscription-dunning-day7",
        "subscription-canceled",
    ] {
        assert!(
            mails_for_user(&app, template, user.id).await.is_empty(),
            "no {template} mail on day 0"
        );
    }
    cleanup_row(&app, sub).await;
}

#[tokio::test]
async fn sequence_day3_day7_suspend_via_test_clock() {
    let _guard = DUNNING_TEST_LOCK.lock().await;
    let clock = test_clock();
    let (app, clock) = dunning_app(clock).await;
    cleanup_stale_past_due(&app).await;
    let user = app.seed_user_unique().await;
    let sub = seed_subscription(&app, user.id, "pro", "active").await;
    fail_payment(&app, sub).await;

    // Day 3: exactly one day-3 reminder, step 3, audited.
    clock.advance(Duration::days(3));
    let report = run_dunning(&app.state).await.expect("job runs");
    assert_eq!(report.reminded_day3, 1);
    assert_eq!(report.reminded_day7, 0);
    assert_eq!(report.suspended, 0);
    let row = row_of(&app, sub).await;
    assert_eq!(row.status, "past_due");
    assert_eq!(row.dunning_step, 3);
    let mails = mails_for_user(&app, "subscription-dunning-day3", user.id).await;
    assert_eq!(mails.len(), 1, "exactly one day-3 mail");
    assert_eq!(
        mails[0].payload,
        serde_json::json!({ "user_id": user.id, "plan_code": "pro" })
    );
    let reminder = audit_rows(&app, sub)
        .await
        .into_iter()
        .find(|a| a.action == DUNNING_REMINDER_ACTION)
        .expect("a reminder audit row");
    assert_eq!(
        reminder.after,
        Some(serde_json::json!({ "dunning_step": 3, "day": 3 }))
    );
    assert_eq!(reminder.reason.as_deref(), Some("dunning-day-3"));
    assert_eq!(reminder.actor_client.as_deref(), Some("dunning"));

    // Day 6: nothing is due.
    clock.advance(Duration::days(3));
    let report = run_dunning(&app.state).await.expect("job runs");
    assert_eq!(report.reminded_day3, 0);
    assert_eq!(report.reminded_day7, 0);
    assert_eq!(report.suspended, 0);
    assert_eq!(
        mails_for_user(&app, "subscription-dunning-day3", user.id)
            .await
            .len(),
        1,
        "still exactly one day-3 mail"
    );

    // Day 7: exactly one day-7 reminder, step 7.
    clock.advance(Duration::days(1));
    let report = run_dunning(&app.state).await.expect("job runs");
    assert_eq!(report.reminded_day7, 1);
    assert_eq!(row_of(&app, sub).await.dunning_step, 7);
    assert_eq!(
        mails_for_user(&app, "subscription-dunning-day7", user.id)
            .await
            .len(),
        1,
        "exactly one day-7 mail"
    );

    // Day 14: the subscription is canceled — the suspend is a
    // status move, so the canceled mail + the status_changed
    // audit land with it, and the user falls back to Free.
    clock.advance(Duration::days(7));
    let report = run_dunning(&app.state).await.expect("job runs");
    assert_eq!(report.suspended, 1);
    let row = row_of(&app, sub).await;
    assert_eq!(row.status, "canceled");
    assert!(row.past_due_since.is_none(), "cancel clears the clock");
    assert_eq!(row.dunning_step, 0);
    assert_eq!(
        mails_for_user(&app, "subscription-canceled", user.id)
            .await
            .len(),
        1,
        "exactly one canceled mail"
    );
    let cancel = audit_rows(&app, sub)
        .await
        .into_iter()
        .find(|a| {
            a.action == "billing.subscription.status_changed"
                && a.after
                    .as_ref()
                    .and_then(|v| v.get("status"))
                    .and_then(|v| v.as_str())
                    == Some("canceled")
        })
        .expect("a cancel audit row");
    assert_eq!(cancel.reason.as_deref(), Some("dunning-day-14"));
    let resolved = effective_plan(&app.state.plans, &app.pool, user.id)
        .await
        .expect("resolve plan");
    assert_eq!(resolved.plan.code, "free");

    // After the cancel there is no past_due row left to scan
    // for this user — and the job sends nothing late.
    let report = run_dunning(&app.state).await.expect("job runs");
    assert_eq!(report.reminded_day3, 0);
    assert_eq!(report.reminded_day7, 0);
    assert_eq!(report.suspended, 0);
    assert_eq!(
        mails_for_user(&app, "subscription-dunning-day3", user.id)
            .await
            .len(),
        1,
        "no late reminders after the cancel"
    );
    cleanup_row(&app, sub).await;
}

#[tokio::test]
async fn rerun_same_day_sends_nothing_twice() {
    let _guard = DUNNING_TEST_LOCK.lock().await;
    let clock = test_clock();
    let (app, clock) = dunning_app(clock).await;
    cleanup_stale_past_due(&app).await;
    let user = app.seed_user_unique().await;
    let sub = seed_subscription(&app, user.id, "pro", "active").await;
    fail_payment(&app, sub).await;

    clock.advance(Duration::days(3));
    let first = run_dunning(&app.state).await.expect("job runs");
    assert_eq!(first.reminded_day3, 1);
    // Same frozen day, same row: the step guard skips it.
    let second = run_dunning(&app.state).await.expect("job runs");
    assert_eq!(second.reminded_day3, 0);
    assert_eq!(
        mails_for_user(&app, "subscription-dunning-day3", user.id)
            .await
            .len(),
        1,
        "exactly one day-3 mail after two passes"
    );
    assert_eq!(audit_rows(&app, sub).await.len(), 2, "status + reminder");
    cleanup_row(&app, sub).await;
}

#[tokio::test]
async fn recovery_mid_sequence_stops_the_reminders() {
    let _guard = DUNNING_TEST_LOCK.lock().await;
    let clock = test_clock();
    let (app, clock) = dunning_app(clock).await;
    cleanup_stale_past_due(&app).await;
    let user = app.seed_user_unique().await;
    let sub = seed_subscription(&app, user.id, "pro", "active").await;
    fail_payment(&app, sub).await;

    // Day 3 mails, then the payment heals on day 5.
    clock.advance(Duration::days(3));
    let report = run_dunning(&app.state).await.expect("job runs");
    assert_eq!(report.reminded_day3, 1);
    clock.advance(Duration::days(2));
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

    // Past the would-be day 14, the job finds nothing to do
    // for this user: the row is active, the dunning state is
    // cleared, and no day-7 mail or cancel ever fires for
    // this failure.
    clock.advance(Duration::days(9));
    let report = run_dunning(&app.state).await.expect("job runs");
    assert_eq!(report.reminded_day3, 0);
    assert_eq!(report.reminded_day7, 0);
    assert_eq!(report.suspended, 0);
    let row = row_of(&app, sub).await;
    assert_eq!(row.status, "active");
    assert!(row.past_due_since.is_none());
    assert_eq!(row.dunning_step, 0);
    assert!(
        mails_for_user(&app, "subscription-dunning-day7", user.id)
            .await
            .is_empty(),
        "no day-7 mail after recovery"
    );
    assert!(
        mails_for_user(&app, "subscription-canceled", user.id)
            .await
            .is_empty(),
        "no cancel after recovery"
    );
    let resolved = effective_plan(&app.state.plans, &app.pool, user.id)
        .await
        .expect("resolve plan");
    assert_eq!(resolved.plan.code, "pro");
    cleanup_row(&app, sub).await;
}

#[tokio::test]
async fn disabled_config_is_a_noop() {
    let _guard = DUNNING_TEST_LOCK.lock().await;
    let clock = test_clock();
    let (app, clock) = dunning_app_with(clock, |s| s.dunning_enabled = false).await;
    cleanup_stale_past_due(&app).await;
    let user = app.seed_user_unique().await;
    let sub = seed_subscription(&app, user.id, "pro", "active").await;
    fail_payment(&app, sub).await;

    clock.advance(Duration::days(14));
    let report = run_dunning(&app.state).await.expect("job runs");
    assert!(!report.ran, "disabled means the job does not run");
    assert_eq!(report.scanned, 0);
    assert_eq!(row_of(&app, sub).await.status, "past_due", "row untouched");
    assert_eq!(
        mails_for_user(&app, "subscription-dunning", user.id)
            .await
            .len(),
        1,
        "only the day-0 mail — the job sent nothing"
    );
    cleanup_row(&app, sub).await;
}

#[tokio::test]
async fn custom_days_follow_config() {
    let _guard = DUNNING_TEST_LOCK.lock().await;
    let clock = test_clock();
    let (app, clock) = dunning_app_with(clock, |s| {
        s.dunning_day3_days = 1;
        s.dunning_day7_days = 2;
        s.dunning_suspend_days = 5;
    })
    .await;
    cleanup_stale_past_due(&app).await;
    let user = app.seed_user_unique().await;
    let sub = seed_subscription(&app, user.id, "pro", "active").await;
    fail_payment(&app, sub).await;

    // The "day-3" template fires on configured day 1.
    clock.advance(Duration::days(1));
    let report = run_dunning(&app.state).await.expect("job runs");
    assert_eq!(report.reminded_day3, 1);
    assert_eq!(row_of(&app, sub).await.dunning_step, 3);
    // The "day-7" template fires on configured day 2.
    clock.advance(Duration::days(1));
    let report = run_dunning(&app.state).await.expect("job runs");
    assert_eq!(report.reminded_day7, 1);
    // The suspend fires on configured day 5.
    clock.advance(Duration::days(3));
    let report = run_dunning(&app.state).await.expect("job runs");
    assert_eq!(report.suspended, 1);
    assert_eq!(row_of(&app, sub).await.status, "canceled");
    cleanup_row(&app, sub).await;
}
