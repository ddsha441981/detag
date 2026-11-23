//! Weekly digest email (P1-101, FR-C-01): one mail per user
//! per week — the week's usage totals, the plan, and tips
//! derived from the numbers — behind the `weekly_digest`
//! feature flag (OFF by default) and the user's opt-out.
//!
//! # How a digest is decided
//!
//! [`send_weekly_digest`] takes the user row (which carries
//! the `settings` jsonb), the user's effective plan (the
//! caller resolves it — the same `effective_plan` the quota
//! gate uses), the week's start date (a Monday, UTC), and
//! the flag as a plain bool (the caller reads it from
//! `settings.feature_flags().weekly_digest` — the service
//! takes the bool so the flag decision is one line in the
//! test, not a settings fixture). The checks run in this
//! order: flag off → [`DigestOutcome::SkippedFlagOff`];
//! opted out → [`DigestOutcome::SkippedOptedOut`]; else the
//! week is read and the mail is enqueued.
//!
//! # The week
//!
//! The week is the 7 UTC days starting at `week_start`
//! ([`usage_counters::for_user_range`] — the same durable
//! rows the usage API reads, never the Redis hash, so the
//! digest agrees with `/v1/usage` by construction). The
//! limits are the plan's DAILY limits × 7 (a plan grants a
//! daily allowance; the weekly allowance is seven days of
//! it). A day with no row contributes 0 — a user who used
//! nothing gets a zero digest, not no digest.
//!
//! # The once/week cap
//!
//! The cap lives in the outbox itself, not in a second table:
//! [`email_outbox::try_enqueue_weekly_digest`] inserts with
//! `ON CONFLICT DO NOTHING` against the partial unique index
//! on (`template`, `payload->>'user_id'`,
//! `payload->>'week_start'`) for the `weekly-digest`
//! template (migration 20261123023000 — the P1-085
//! quota-warning shape). The insert IS the check, so two
//! weekly-job runs racing the same week enqueue exactly one
//! mail — no advisory lock, no read-then-write race. Next
//! week is a new `week_start`, so the cap resets with the
//! week.
//!
//! # The opt-out
//!
//! `users.settings` jsonb, key `weekly_digest`: `false`
//! means opted out, anything else (absent, `true`, a
//! non-bool) means opted in — the default is ON, the flag
//! is the OFF switch (ASSUMPTION-062). This is the first
//! consumer of the raw `settings` column; P1-103 owns the
//! general prefs system and may namespace or migrate the
//! key. There is no self-serve toggle endpoint today (no
//! PATCH /v1/me settings field exists), so the opt-out is
//! honored by the sender and settable via the row until
//! P1-103 lands the prefs endpoint.
//!
//! # The tips
//!
//! [`tips_for`] is pure: the tips follow from the numbers
//! alone, so they are unit-tested without a database. A tip
//! is a suggestion, never a claim about a past result — the
//! copy rule (05-compliance-aup.md §6) applies here too:
//! no "invisible", no "undetectable", no SynthID.
//!
//! # No audit row
//!
//! A digest is a routine notification, not an anomaly — the
//! outbox row itself is the record (support reads it per
//! user + template, the same way the resend limit does), so
//! no `audit_logs` row is written (the P1-085 precedent).
//!
//! # Invocation
//!
//! No production caller exists yet: the weekly schedule
//! does not exist (the worker's nightly schedule enqueues
//! `reconciliation` only, and the digest's kind
//! `weekly_digest` is a stub there — the P1-070 dunning
//! shape). The Phase 4 console / a weekly schedule calls
//! `send_weekly_digest` per user. Until then the mechanism
//! and the proof live here, and the tests call it directly
//! with seeded usage rows.

use chrono::{Days, NaiveDate};
use detag_core::{
    checksum::sha256_hex,
    error::AppError,
    plans::Entitlements,
    repos::{
        email_outbox::{self, NewOutboxMail},
        plans::Plan,
        usage_counters,
        users::User,
    },
};
use serde_json::{json, Value};
use uuid::Uuid;

/// The weekly-digest template.
pub const WEEKLY_DIGEST_TEMPLATE: &str = "weekly-digest";

/// What sending a digest did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DigestOutcome {
    /// The mail was enqueued (carries the outbox row id).
    Sent { mail_id: Uuid },
    /// The flag is off — nothing was enqueued, no matter
    /// what the user opted.
    SkippedFlagOff,
    /// The user opted out — nothing was enqueued.
    SkippedOptedOut,
    /// A digest for this user + week is already queued or
    /// sent — the once/week cap held.
    AlreadySent,
}

/// Whether the user opted out of the weekly digest: the
/// `weekly_digest` key in `users.settings` is exactly
/// `false`. Absent, `true`, or a non-bool value means opted
/// in — the default is ON, the flag is the OFF switch.
pub fn weekly_digest_opted_out(settings: &Value) -> bool {
    settings.get("weekly_digest") == Some(&Value::Bool(false))
}

/// Tips for the digest, from the week's numbers alone.
/// At most three, in priority order: near-limit first,
/// then the no-video plan note, then the no-use note — and
/// the API-key tip when nothing else applies (a digest with
/// no tip would render an empty list).
pub fn tips_for(
    images_used: i64,
    images_limit: i64,
    video_seconds_limit: i64,
) -> Vec<&'static str> {
    let mut tips = Vec::new();
    if images_limit > 0 && images_used * 100 >= 80 * images_limit {
        tips.push(
            "You used most of your weekly image allowance — your daily quota resets at midnight UTC.",
        );
    }
    if video_seconds_limit == 0 {
        tips.push(
            "Your plan includes no video seconds — upgrade to Pro or Business to process video.",
        );
    }
    if images_used == 0 {
        tips.push(
            "You didn't process any images this week — your first upload is ready when you are.",
        );
    }
    if tips.is_empty() {
        tips.push(
            "Create an API key from your account settings to automate uploads from your own tools.",
        );
    }
    tips
}

/// Send (enqueue) the weekly digest for one user, or skip it.
/// The week is the 7 UTC days starting at `week_start`;
/// `weekly_digest_enabled` is the `weekly_digest` feature
/// flag (OFF by default — the caller reads it from settings).
pub async fn send_weekly_digest(
    pool: &detag_core::db::DbPool,
    user: &User,
    plan: &Plan,
    week_start: NaiveDate,
    weekly_digest_enabled: bool,
) -> Result<DigestOutcome, AppError> {
    if !weekly_digest_enabled {
        return Ok(DigestOutcome::SkippedFlagOff);
    }
    if weekly_digest_opted_out(&user.settings) {
        return Ok(DigestOutcome::SkippedOptedOut);
    }
    let week_end = week_start
        .checked_add_days(Days::new(6))
        .expect("a 7-day week from any real date cannot overflow");
    let rows = usage_counters::for_user_range(pool, user.id, week_start, week_end)
        .await
        .map_err(|_| AppError::DatabaseUnavailable)?;
    let images_used: i64 = rows.iter().map(|row| i64::from(row.images_used)).sum();
    let video_seconds_used: i64 = rows
        .iter()
        .map(|row| i64::from(row.video_seconds_used))
        .sum();
    let entitlements = Entitlements::from_value(&plan.entitlements);
    let images_limit = entitlements.daily_images * 7;
    let video_seconds_limit = entitlements.daily_video_seconds * 7;
    let tips = tips_for(images_used, images_limit, video_seconds_limit);
    let payload = json!({
        "user_id": user.id.to_string(),
        "week_start": week_start.to_string(),
        "images_used": images_used,
        "images_limit": images_limit,
        "video_seconds_used": video_seconds_used,
        "video_seconds_limit": video_seconds_limit,
        "plan_code": plan.code,
        "tips": tips,
    });
    let mail = NewOutboxMail {
        to_hash: sha256_hex(user.email.as_bytes()),
        template: WEEKLY_DIGEST_TEMPLATE.to_string(),
        payload,
        scheduled_at: None,
    };
    match email_outbox::try_enqueue_weekly_digest(pool, &mail)
        .await
        .map_err(|_| AppError::Internal)?
    {
        Some(mail_id) => Ok(DigestOutcome::Sent { mail_id }),
        None => Ok(DigestOutcome::AlreadySent),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opted_out_only_when_the_key_is_exactly_false() {
        assert!(!weekly_digest_opted_out(&json!({})));
        assert!(!weekly_digest_opted_out(&json!({"weekly_digest": true})));
        assert!(weekly_digest_opted_out(&json!({"weekly_digest": false})));
        // A non-bool value is not an opt-out — the key is
        // mistyped, not a choice; the default (opted in) wins.
        assert!(!weekly_digest_opted_out(&json!({"weekly_digest": "no"})));
        assert!(!weekly_digest_opted_out(&json!({"weekly_digest": 0})));
        assert!(!weekly_digest_opted_out(&Value::Null));
        assert!(!weekly_digest_opted_out(&json!({"other": 1})));
    }

    #[test]
    fn tips_follow_the_numbers() {
        // Free week, nothing used: the no-use note + the
        // no-video plan note (the plan, not the usage).
        assert_eq!(
            tips_for(0, 70, 0),
            vec![
                "Your plan includes no video seconds — upgrade to Pro or Business to process video.",
                "You didn't process any images this week — your first upload is ready when you are.",
            ]
        );
        // Near the weekly image allowance: the near-limit tip
        // leads, the no-video note follows.
        assert_eq!(
            tips_for(56, 70, 0),
            vec![
                "You used most of your weekly image allowance — your daily quota resets at midnight UTC.",
                "Your plan includes no video seconds — upgrade to Pro or Business to process video.",
            ]
        );
        // Comfortable use on a plan with video: the API-key
        // tip is the only one that applies.
        assert_eq!(
            tips_for(5, 70, 25_200),
            vec!["Create an API key from your account settings to automate uploads from your own tools."]
        );
        // Exactly 80% trips the near-limit tip (at-or-above,
        // the quota-warning precedent).
        assert_eq!(
            tips_for(56, 70, 25_200),
            vec!["You used most of your weekly image allowance — your daily quota resets at midnight UTC."]
        );
    }
}
