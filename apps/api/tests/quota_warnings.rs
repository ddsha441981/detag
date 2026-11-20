//! P1-085: quota warning emails — live, against the real
//! outbox (FR-P-07).
//!
//! The service decides from the durable snapshot (the same
//! one the quota gate reads) and enqueues the warning mail
//! into `email_outbox`; the once/day cap is the partial
//! unique index on (user, day) per template, so the tests
//! prove the cap by reading the outbox back:
//!
//! - `warn_at_80_enqueues_once` — a Free user at 8/10 images
//!   gets exactly one `quota-warning-80` row, with the
//!   payload the template renders (user, day, dimension,
//!   used/limit, percent, plan).
//! - `second_call_same_day_sends_nothing_twice` — the AC: a
//!   second check at the same usage the same day returns
//!   `AlreadyWarned` and the outbox still holds exactly one
//!   row.
//! - `full_at_100_enqueues_100_template` — at 10/10 the
//!   `quota-warning-100` mail lands, and an 80% mail already
//!   sent the same day does NOT block it (the caps are per
//!   level).
//! - `jump_straight_to_full_sends_only_full` — usage jumping
//!   from 0 to 10/10 in one check sends only the 100% mail,
//!   never the 80% one.
//! - `below_threshold_sends_nothing` — 7/10 mails nothing.
//! - `next_day_cap_resets` — the same user at 8/10 the next
//!   day gets a second 80% mail (the cap key is the day).
//! - `zero_limit_video_dimension_ignored` — a Free user with
//!   video usage but a 0 video limit gets no warning (0 is
//!   "not granted", not a percent).
//! - `video_dimension_trips_for_pro` — a Pro user at 2880 of
//!   3600 video-seconds gets the 80% mail naming the video
//!   leg.
//! - `disabled_config_is_a_noop` — `enabled=false` mails
//!   nothing even at 10/10.
//! - `concurrent_calls_enqueue_once` — two checks racing the
//!   same threshold land exactly one row: the unique index
//!   makes the cap hold under concurrency, not just in
//!   sequence.
//!
//! Isolation: every test seeds its own fresh user (UUID), so
//! the shared harness DB is not a boundary here — outbox and
//! usage rows are per-user, and no other suite enqueues the
//! quota-warning templates. The days are fixed (2026-11-20 /
//! 21), seeded directly — no frozen clock is needed because
//! the service takes the day explicitly.

//! - `enqueued_mail_renders` — the render contract: the
//!   enqueued row's payload, plus exactly the two variables
//!   the drain adds (`base_url` / `app_name`), renders both
//!   templates. The templates use `{{ unit }}`, so a payload
//!   without it would fail loudly at drain time and park the
//!   mail unsent — this test pins the service side of that
//!   contract, not just the outbox row.

mod common;

use chrono::NaiveDate;
use common::{spawn_app, TestApp};
use detag_api::services::quota_warnings::{
    maybe_warn_quota, QuotaWarningConfig, WarnOutcome, QUOTA_WARNING_100_TEMPLATE,
    QUOTA_WARNING_80_TEMPLATE,
};
use detag_core::repos::plans::Plan;
use detag_core::repos::{email_outbox, plans};
use detag_mailer::TemplateEngine;
use serde_json::json;
use uuid::Uuid;

fn day() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 11, 20).expect("fixed test day")
}

fn next_day() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 11, 21).expect("fixed next day")
}

fn config() -> QuotaWarningConfig {
    QuotaWarningConfig {
        enabled: true,
        warn_pct: 80,
        full_pct: 100,
    }
}

/// Seed a usage row for the user on `day` directly (the
/// P1-005 helper shape — the meter has no production caller
/// yet, so the tests seed the row the snapshot reads).
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

/// Raise the user's images on `day` to `images` (the 80→100
/// step of the full test).
async fn set_images(app: &TestApp, user_id: Uuid, on: NaiveDate, images: i32) {
    sqlx::query(
        "UPDATE usage_counters SET images_used = $3 \
         WHERE user_id = $1 AND day = $2",
    )
    .bind(user_id)
    .bind(on)
    .bind(images)
    .execute(&app.pool)
    .await
    .expect("raise usage");
}

async fn plan(app: &TestApp, code: &str) -> Plan {
    plans::by_code(&app.pool, code)
        .await
        .expect("load plan")
        .expect("plan is seeded")
}

/// This user's outbox rows for one template (sent or unsent
/// — nothing is drained in these tests, so unsent is the
/// whole set; the cap counts sent rows too, which is what
/// the second-call tests prove via the outcome).
async fn mails_for_user(
    app: &TestApp,
    user_id: Uuid,
    template: &str,
) -> Vec<email_outbox::OutboxMail> {
    email_outbox::unsent_for_template(&app.pool, template)
        .await
        .expect("read outbox")
        .into_iter()
        .filter(|row| {
            row.payload.get("user_id").and_then(|v| v.as_str()) == Some(&user_id.to_string())
        })
        .collect()
}

#[tokio::test]
async fn warn_at_80_enqueues_once() {
    let app = spawn_app().await;
    let user = app.seed_user_unique().await;
    let free = plan(&app, "free").await;
    seed_usage(&app, user.id, day(), 8, 0).await;

    let outcome = maybe_warn_quota(&app.pool, &config(), user.id, &free, day())
        .await
        .expect("warn check runs");
    assert!(
        matches!(outcome, WarnOutcome::Sent { .. }),
        "8/10 must send: {outcome:?}"
    );

    let mails = mails_for_user(&app, user.id, QUOTA_WARNING_80_TEMPLATE).await;
    assert_eq!(mails.len(), 1, "exactly one 80% mail");
    let payload = &mails[0].payload;
    assert_eq!(payload["user_id"], user.id.to_string());
    assert_eq!(payload["day"], "2026-11-20");
    assert_eq!(payload["dimension"], "images");
    assert_eq!(payload["unit"], "images");
    assert_eq!(payload["used"], 8);
    assert_eq!(payload["limit"], 10);
    assert_eq!(payload["percent"], 80);
    assert_eq!(payload["plan_code"], "free");
}

#[tokio::test]
async fn second_call_same_day_sends_nothing_twice() {
    let app = spawn_app().await;
    let user = app.seed_user_unique().await;
    let free = plan(&app, "free").await;
    seed_usage(&app, user.id, day(), 8, 0).await;

    let first = maybe_warn_quota(&app.pool, &config(), user.id, &free, day())
        .await
        .expect("first warn check runs");
    assert!(matches!(first, WarnOutcome::Sent { .. }));
    let second = maybe_warn_quota(&app.pool, &config(), user.id, &free, day())
        .await
        .expect("second warn check runs");
    assert!(
        matches!(second, WarnOutcome::AlreadyWarned { .. }),
        "second call same day must be capped: {second:?}"
    );

    let mails = mails_for_user(&app, user.id, QUOTA_WARNING_80_TEMPLATE).await;
    assert_eq!(mails.len(), 1, "the cap is one mail per user/day/level");
}

#[tokio::test]
async fn full_at_100_enqueues_100_template() {
    let app = spawn_app().await;
    let user = app.seed_user_unique().await;
    let free = plan(&app, "free").await;
    seed_usage(&app, user.id, day(), 8, 0).await;

    let first = maybe_warn_quota(&app.pool, &config(), user.id, &free, day())
        .await
        .expect("warn check runs");
    assert!(matches!(first, WarnOutcome::Sent { .. }));
    set_images(&app, user.id, day(), 10).await;
    let second = maybe_warn_quota(&app.pool, &config(), user.id, &free, day())
        .await
        .expect("full check runs");
    assert!(
        matches!(second, WarnOutcome::Sent { template, .. } if template == QUOTA_WARNING_100_TEMPLATE),
        "10/10 must send the 100% mail: {second:?}"
    );

    assert_eq!(
        mails_for_user(&app, user.id, QUOTA_WARNING_80_TEMPLATE)
            .await
            .len(),
        1
    );
    let full = mails_for_user(&app, user.id, QUOTA_WARNING_100_TEMPLATE).await;
    assert_eq!(full.len(), 1, "exactly one 100% mail");
    assert_eq!(full[0].payload["dimension"], "images");
    assert_eq!(full[0].payload["percent"], 100);
}

#[tokio::test]
async fn jump_straight_to_full_sends_only_full() {
    let app = spawn_app().await;
    let user = app.seed_user_unique().await;
    let free = plan(&app, "free").await;
    seed_usage(&app, user.id, day(), 10, 0).await;

    let outcome = maybe_warn_quota(&app.pool, &config(), user.id, &free, day())
        .await
        .expect("warn check runs");
    assert!(
        matches!(outcome, WarnOutcome::Sent { template, .. } if template == QUOTA_WARNING_100_TEMPLATE),
        "0 → 10/10 must send only the 100% mail: {outcome:?}"
    );
    assert_eq!(
        mails_for_user(&app, user.id, QUOTA_WARNING_80_TEMPLATE)
            .await
            .len(),
        0,
        "no 80% mail when usage jumps straight past it"
    );
    assert_eq!(
        mails_for_user(&app, user.id, QUOTA_WARNING_100_TEMPLATE)
            .await
            .len(),
        1
    );
}

#[tokio::test]
async fn below_threshold_sends_nothing() {
    let app = spawn_app().await;
    let user = app.seed_user_unique().await;
    let free = plan(&app, "free").await;
    seed_usage(&app, user.id, day(), 7, 0).await;

    let outcome = maybe_warn_quota(&app.pool, &config(), user.id, &free, day())
        .await
        .expect("warn check runs");
    assert_eq!(outcome, WarnOutcome::BelowThreshold);
    assert_eq!(
        mails_for_user(&app, user.id, QUOTA_WARNING_80_TEMPLATE)
            .await
            .len(),
        0
    );
    assert_eq!(
        mails_for_user(&app, user.id, QUOTA_WARNING_100_TEMPLATE)
            .await
            .len(),
        0
    );
}

#[tokio::test]
async fn next_day_cap_resets() {
    let app = spawn_app().await;
    let user = app.seed_user_unique().await;
    let free = plan(&app, "free").await;
    seed_usage(&app, user.id, day(), 8, 0).await;
    seed_usage(&app, user.id, next_day(), 8, 0).await;

    let first = maybe_warn_quota(&app.pool, &config(), user.id, &free, day())
        .await
        .expect("day-1 warn check runs");
    assert!(matches!(first, WarnOutcome::Sent { .. }));
    let second = maybe_warn_quota(&app.pool, &config(), user.id, &free, next_day())
        .await
        .expect("day-2 warn check runs");
    assert!(
        matches!(second, WarnOutcome::Sent { .. }),
        "the next day is a new cap key: {second:?}"
    );

    let mails = mails_for_user(&app, user.id, QUOTA_WARNING_80_TEMPLATE).await;
    assert_eq!(mails.len(), 2, "one 80% mail per day");
    let days: Vec<&str> = mails
        .iter()
        .map(|m| m.payload["day"].as_str().expect("day is a string"))
        .collect();
    assert!(days.contains(&"2026-11-20"));
    assert!(days.contains(&"2026-11-21"));
}

#[tokio::test]
async fn zero_limit_video_dimension_ignored() {
    let app = spawn_app().await;
    let user = app.seed_user_unique().await;
    let free = plan(&app, "free").await;
    // Free grants no video-seconds at all — 500 used against a
    // 0 limit must not warn (0 is "not granted", not a percent).
    seed_usage(&app, user.id, day(), 0, 500).await;

    let outcome = maybe_warn_quota(&app.pool, &config(), user.id, &free, day())
        .await
        .expect("warn check runs");
    assert_eq!(outcome, WarnOutcome::BelowThreshold);
    assert_eq!(
        mails_for_user(&app, user.id, QUOTA_WARNING_80_TEMPLATE)
            .await
            .len(),
        0
    );
}

#[tokio::test]
async fn video_dimension_trips_for_pro() {
    let app = spawn_app().await;
    let user = app.seed_user_unique().await;
    let pro = plan(&app, "pro").await;
    // Pro: 3600 video-seconds/day (FR-P-01) — 2880 is 80%.
    seed_usage(&app, user.id, day(), 0, 2880).await;

    let outcome = maybe_warn_quota(&app.pool, &config(), user.id, &pro, day())
        .await
        .expect("warn check runs");
    assert!(
        matches!(outcome, WarnOutcome::Sent { .. }),
        "2880/3600 must send: {outcome:?}"
    );

    let mails = mails_for_user(&app, user.id, QUOTA_WARNING_80_TEMPLATE).await;
    assert_eq!(mails.len(), 1);
    assert_eq!(mails[0].payload["dimension"], "video_seconds");
    assert_eq!(mails[0].payload["unit"], "video-seconds");
    assert_eq!(mails[0].payload["used"], 2880);
    assert_eq!(mails[0].payload["limit"], 3600);
    assert_eq!(mails[0].payload["percent"], 80);
    assert_eq!(mails[0].payload["plan_code"], "pro");
}

#[tokio::test]
async fn disabled_config_is_a_noop() {
    let app = spawn_app().await;
    let user = app.seed_user_unique().await;
    let free = plan(&app, "free").await;
    seed_usage(&app, user.id, day(), 10, 0).await;

    let disabled = QuotaWarningConfig {
        enabled: false,
        ..config()
    };
    let outcome = maybe_warn_quota(&app.pool, &disabled, user.id, &free, day())
        .await
        .expect("warn check runs");
    assert_eq!(outcome, WarnOutcome::Disabled);
    assert_eq!(
        mails_for_user(&app, user.id, QUOTA_WARNING_100_TEMPLATE)
            .await
            .len(),
        0
    );
}

#[tokio::test]
async fn concurrent_calls_enqueue_once() {
    let app = spawn_app().await;
    let user = app.seed_user_unique().await;
    let free = plan(&app, "free").await;
    seed_usage(&app, user.id, day(), 8, 0).await;

    let cfg = config();
    let (first, second) = tokio::join!(
        maybe_warn_quota(&app.pool, &cfg, user.id, &free, day()),
        maybe_warn_quota(&app.pool, &cfg, user.id, &free, day()),
    );
    let first = first.expect("first racing check runs");
    let second = second.expect("second racing check runs");
    // Exactly one of the two sent; the other was capped — in
    // either order, the outbox holds one row.
    assert!(
        matches!(first, WarnOutcome::Sent { .. }) != matches!(second, WarnOutcome::Sent { .. }),
        "one Sent + one AlreadyWarned, got {first:?} / {second:?}"
    );
    assert_eq!(
        mails_for_user(&app, user.id, QUOTA_WARNING_80_TEMPLATE)
            .await
            .len(),
        1,
        "the unique index caps concurrent checks to one mail"
    );
}

/// Render one enqueued row exactly as the drain would: the
/// row's payload plus the drain's own `base_url` / `app_name`
/// (drain.rs) — nothing else. A missing template variable
/// fails loudly here, the same way it would park the row.
fn render_as_drain(row: &email_outbox::OutboxMail) -> detag_mailer::RenderedMail {
    let engine = TemplateEngine::new().expect("templates compile");
    let mut ctx = row.payload.clone();
    let obj = ctx.as_object_mut().expect("payload is an object");
    obj.insert("base_url".to_string(), json!("https://example.invalid"));
    obj.insert("app_name".to_string(), json!("Detag"));
    engine
        .render(&row.template, &ctx)
        .expect("the enqueued payload renders its template")
}

#[tokio::test]
async fn enqueued_mail_renders() {
    let app = spawn_app().await;
    let user = app.seed_user_unique().await;
    let free = plan(&app, "free").await;
    seed_usage(&app, user.id, day(), 8, 0).await;

    let first = maybe_warn_quota(&app.pool, &config(), user.id, &free, day())
        .await
        .expect("warn check runs");
    assert!(matches!(first, WarnOutcome::Sent { .. }));
    let mails = mails_for_user(&app, user.id, QUOTA_WARNING_80_TEMPLATE).await;
    assert_eq!(mails.len(), 1);
    let rendered = render_as_drain(&mails[0]);
    assert!(
        rendered.subject.contains("80%"),
        "80% subject: {}",
        rendered.subject
    );
    assert!(
        rendered.text_body.contains("8 of 10 images"),
        "80% body states the numbers: {}",
        rendered.text_body
    );

    set_images(&app, user.id, day(), 10).await;
    let second = maybe_warn_quota(&app.pool, &config(), user.id, &free, day())
        .await
        .expect("full check runs");
    assert!(matches!(second, WarnOutcome::Sent { .. }));
    let full = mails_for_user(&app, user.id, QUOTA_WARNING_100_TEMPLATE).await;
    assert_eq!(full.len(), 1);
    let rendered = render_as_drain(&full[0]);
    assert!(
        rendered.subject.contains("quota is full"),
        "100% subject: {}",
        rendered.subject
    );
    assert!(
        rendered.text_body.contains("10 of 10 images"),
        "100% body states the numbers: {}",
        rendered.text_body
    );
}
