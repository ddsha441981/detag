//! P1-101: weekly digest email — live, against the real
//! outbox (FR-C-01).
//!
//! The service decides from the flag + the opt-out + the
//! durable usage rows for the week, and enqueues the digest
//! into `email_outbox`; the once/week cap is the partial
//! unique index on (user, week_start) for the template, so
//! the tests prove the cap by reading the outbox back:
//!
//! - `flag_off_sends_nothing` — the AC: the flag is the OFF
//!   switch — a user with usage and no opt-out gets nothing
//!   while `PF_FEATURE_WEEKLY_DIGEST` is off.
//! - `opted_out_user_gets_nothing` — flag on, `settings`
//!   `{"weekly_digest": false}` → no mail, even with usage.
//! - `opted_in_user_gets_one_digest_with_the_week_totals` —
//!   flag on, no opt-out: exactly one `weekly-digest` row,
//!   with the payload the template renders (user, week_start,
//!   week totals, weekly limits, plan, tips) — and a usage
//!   row from the week BEFORE is not counted.
//! - `second_call_same_week_sends_nothing_twice` — the AC
//!   for the cap: a second call the same week returns
//!   `AlreadySent` and the outbox still holds exactly one row.
//! - `next_week_sends_again` — the same user the next week
//!   gets a second digest (the cap key is the week).
//! - `zero_usage_week_still_digests` — a week with no usage
//!   rows still digests (zeros + the no-use tip), not silence.
//! - `concurrent_calls_enqueue_once` — two calls racing the
//!   same week land exactly one row: the unique index makes
//!   the cap hold under concurrency, not just in sequence.
//!
//! - `enqueued_mail_renders` — the render contract: the
//!   enqueued row's payload, plus exactly the two variables
//!   the drain adds (`base_url` / `app_name`), renders the
//!   template. The template uses `{{ week_start }}` / `{{ tips
//!   }}` / the totals, so a payload without one would fail
//!   loudly at drain time and park the mail unsent — this test
//!   pins the service side of that contract, not just the
//!   outbox row.
//!
//! Isolation: every test seeds its own fresh user (UUID), so
//! the shared harness DB is not a boundary here — outbox and
//! usage rows are per-user, and no other suite enqueues the
//! `weekly-digest` template. The weeks are fixed
//! (2026-11-16 / 23, both Mondays), seeded directly — no
//! frozen clock is needed because the service takes the week
//! explicitly.

mod common;

use chrono::NaiveDate;
use common::{spawn_app, TestApp};
use detag_api::services::weekly_digest::{
    send_weekly_digest, DigestOutcome, WEEKLY_DIGEST_TEMPLATE,
};
use detag_core::repos::plans::Plan;
use detag_core::repos::{email_outbox, plans, users};
use detag_mailer::TemplateEngine;
use serde_json::json;
use uuid::Uuid;

fn week() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 11, 16).expect("fixed test week (a Monday)")
}

fn next_week() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 11, 23).expect("fixed next week (a Monday)")
}

/// Seed a usage row for the user on `day` directly (the
/// P1-085 helper shape — the meter has no production caller
/// yet, so the tests seed the rows the digest sums).
async fn seed_usage(app: &TestApp, user_id: Uuid, on: NaiveDate, images: i32, video_seconds: i32) {
    sqlx::query(
        "INSERT INTO usage_counters (user_id, day, images_used, video_seconds_used) \
         VALUES ($1, $2, $3, $4)",
    )
    .bind(user_id)
    .bind(on)
    .bind(images)
    .bind(video_seconds)
    .execute(&app.pool)
    .await
    .expect("seed usage");
}

/// Set the user's `settings` jsonb (the opt-out lives there;
/// no PATCH /v1/me settings field exists yet — P1-103 owns
/// the prefs endpoint — so the tests write the row directly).
async fn set_settings(app: &TestApp, user_id: Uuid, settings: serde_json::Value) {
    sqlx::query("UPDATE users SET settings = $2 WHERE id = $1")
        .bind(user_id)
        .bind(settings)
        .execute(&app.pool)
        .await
        .expect("set settings");
}

async fn user(app: &TestApp, user_id: Uuid) -> users::User {
    users::by_id(&app.pool, user_id)
        .await
        .expect("read user")
        .expect("user is seeded")
}

async fn plan(app: &TestApp, code: &str) -> Plan {
    plans::by_code(&app.pool, code)
        .await
        .expect("load plan")
        .expect("plan is seeded")
}

/// This user's outbox rows for the weekly digest (sent or
/// unsent — nothing is drained in these tests, so unsent is
/// the whole set; the cap counts sent rows too, which is what
/// the second-call tests prove via the outcome).
async fn mails_for_user(app: &TestApp, user_id: Uuid) -> Vec<email_outbox::OutboxMail> {
    email_outbox::unsent_for_template(&app.pool, WEEKLY_DIGEST_TEMPLATE)
        .await
        .expect("read outbox")
        .into_iter()
        .filter(|row| {
            row.payload.get("user_id").and_then(|v| v.as_str()) == Some(&user_id.to_string())
        })
        .collect()
}

#[tokio::test]
async fn flag_off_sends_nothing() {
    let app = spawn_app().await;
    let seeded = app.seed_user_unique().await;
    let free = plan(&app, "free").await;
    seed_usage(&app, seeded.id, week(), 12, 0).await;

    let outcome = send_weekly_digest(
        &app.pool,
        &user(&app, seeded.id).await,
        &free,
        week(),
        false,
    )
    .await
    .expect("digest runs");
    assert!(
        matches!(outcome, DigestOutcome::SkippedFlagOff),
        "flag off must skip: {outcome:?}"
    );
    assert_eq!(mails_for_user(&app, seeded.id).await.len(), 0);
}

#[tokio::test]
async fn opted_out_user_gets_nothing() {
    let app = spawn_app().await;
    let seeded = app.seed_user_unique().await;
    let free = plan(&app, "free").await;
    seed_usage(&app, seeded.id, week(), 12, 0).await;
    set_settings(&app, seeded.id, json!({"weekly_digest": false})).await;

    let outcome = send_weekly_digest(&app.pool, &user(&app, seeded.id).await, &free, week(), true)
        .await
        .expect("digest runs");
    assert!(
        matches!(outcome, DigestOutcome::SkippedOptedOut),
        "an opted-out user must be skipped: {outcome:?}"
    );
    assert_eq!(mails_for_user(&app, seeded.id).await.len(), 0);
}

#[tokio::test]
async fn opted_in_user_gets_one_digest_with_the_week_totals() {
    let app = spawn_app().await;
    let seeded = app.seed_user_unique().await;
    let free = plan(&app, "free").await;
    // Two days inside the week + one day from the week
    // before (100 images that must NOT be counted).
    seed_usage(&app, seeded.id, week(), 5, 0).await;
    seed_usage(
        &app,
        seeded.id,
        NaiveDate::from_ymd_opt(2026, 11, 18).expect("Wednesday of the week"),
        7,
        0,
    )
    .await;
    seed_usage(
        &app,
        seeded.id,
        NaiveDate::from_ymd_opt(2026, 11, 15).expect("Sunday before the week"),
        100,
        0,
    )
    .await;

    let outcome = send_weekly_digest(&app.pool, &user(&app, seeded.id).await, &free, week(), true)
        .await
        .expect("digest runs");
    assert!(
        matches!(outcome, DigestOutcome::Sent { .. }),
        "an opted-in user must get the digest: {outcome:?}"
    );

    let mails = mails_for_user(&app, seeded.id).await;
    assert_eq!(mails.len(), 1, "exactly one digest");
    let payload = &mails[0].payload;
    assert_eq!(payload["user_id"], seeded.id.to_string());
    assert_eq!(payload["week_start"], "2026-11-16");
    assert_eq!(payload["images_used"], 12);
    assert_eq!(payload["images_limit"], 70);
    assert_eq!(payload["video_seconds_used"], 0);
    assert_eq!(payload["video_seconds_limit"], 0);
    assert_eq!(payload["plan_code"], "free");
    assert_eq!(
        payload["tips"],
        json!([
            "Your plan includes no video seconds — upgrade to Pro or Business to process video."
        ])
    );
}

#[tokio::test]
async fn second_call_same_week_sends_nothing_twice() {
    let app = spawn_app().await;
    let seeded = app.seed_user_unique().await;
    let free = plan(&app, "free").await;
    seed_usage(&app, seeded.id, week(), 12, 0).await;

    let first = send_weekly_digest(&app.pool, &user(&app, seeded.id).await, &free, week(), true)
        .await
        .expect("first digest runs");
    assert!(matches!(first, DigestOutcome::Sent { .. }));
    let second = send_weekly_digest(&app.pool, &user(&app, seeded.id).await, &free, week(), true)
        .await
        .expect("second digest runs");
    assert!(
        matches!(second, DigestOutcome::AlreadySent),
        "second call same week must be capped: {second:?}"
    );

    let mails = mails_for_user(&app, seeded.id).await;
    assert_eq!(mails.len(), 1, "the cap is one mail per user/week");
}

#[tokio::test]
async fn next_week_sends_again() {
    let app = spawn_app().await;
    let seeded = app.seed_user_unique().await;
    let free = plan(&app, "free").await;
    seed_usage(&app, seeded.id, week(), 12, 0).await;

    let first = send_weekly_digest(&app.pool, &user(&app, seeded.id).await, &free, week(), true)
        .await
        .expect("first digest runs");
    assert!(matches!(first, DigestOutcome::Sent { .. }));
    let second = send_weekly_digest(
        &app.pool,
        &user(&app, seeded.id).await,
        &free,
        next_week(),
        true,
    )
    .await
    .expect("second digest runs");
    assert!(
        matches!(second, DigestOutcome::Sent { .. }),
        "next week is a new cap key: {second:?}"
    );

    let mails = mails_for_user(&app, seeded.id).await;
    assert_eq!(mails.len(), 2, "one digest per week");
}

#[tokio::test]
async fn zero_usage_week_still_digests() {
    let app = spawn_app().await;
    let seeded = app.seed_user_unique().await;
    let free = plan(&app, "free").await;

    let outcome = send_weekly_digest(&app.pool, &user(&app, seeded.id).await, &free, week(), true)
        .await
        .expect("digest runs");
    assert!(
        matches!(outcome, DigestOutcome::Sent { .. }),
        "a zero week still digests: {outcome:?}"
    );

    let mails = mails_for_user(&app, seeded.id).await;
    assert_eq!(mails.len(), 1);
    assert_eq!(mails[0].payload["images_used"], 0);
    assert_eq!(mails[0].payload["images_limit"], 70);
    assert_eq!(
        mails[0].payload["tips"],
        json!([
            "Your plan includes no video seconds — upgrade to Pro or Business to process video.",
            "You didn't process any images this week — your first upload is ready when you are."
        ])
    );
}

#[tokio::test]
async fn concurrent_calls_enqueue_once() {
    let app = spawn_app().await;
    let seeded = app.seed_user_unique().await;
    let free = plan(&app, "free").await;
    seed_usage(&app, seeded.id, week(), 12, 0).await;
    let me = user(&app, seeded.id).await;

    let (first, second) = tokio::join!(
        send_weekly_digest(&app.pool, &me, &free, week(), true),
        send_weekly_digest(&app.pool, &me, &free, week(), true),
    );
    let first = first.expect("first concurrent digest runs");
    let second = second.expect("second concurrent digest runs");
    assert!(
        matches!(first, DigestOutcome::Sent { .. }) && matches!(second, DigestOutcome::AlreadySent)
            || matches!(first, DigestOutcome::AlreadySent)
                && matches!(second, DigestOutcome::Sent { .. }),
        "one Sent + one AlreadySent, got {first:?} / {second:?}"
    );

    let mails = mails_for_user(&app, seeded.id).await;
    assert_eq!(mails.len(), 1, "the cap holds under concurrency");
}

#[tokio::test]
async fn enqueued_mail_renders() {
    let app = spawn_app().await;
    let seeded = app.seed_user_unique().await;
    let free = plan(&app, "free").await;
    seed_usage(&app, seeded.id, week(), 12, 0).await;

    let outcome = send_weekly_digest(&app.pool, &user(&app, seeded.id).await, &free, week(), true)
        .await
        .expect("digest runs");
    assert!(matches!(outcome, DigestOutcome::Sent { .. }));

    let mails = mails_for_user(&app, seeded.id).await;
    assert_eq!(mails.len(), 1);
    let mut context = mails[0].payload.clone();
    context["base_url"] = json!("http://localhost:8080");
    context["app_name"] = json!("Detag");
    let engine = TemplateEngine::new().expect("templates compile");
    let rendered = engine
        .render(WEEKLY_DIGEST_TEMPLATE, &context)
        .expect("the enqueued payload must render");
    assert_eq!(rendered.subject, "Your week on Detag");
    assert!(rendered.text_body.contains("Images processed: 12 of 70"));
    assert!(rendered.text_body.contains("Plan: free"));
    assert!(rendered
        .text_body
        .contains("Your plan includes no video seconds"));
}
