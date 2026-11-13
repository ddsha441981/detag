//! detag-mailer::drain — the outbox drain (P1-028).
//!
//! One drain pass claims up to `batch` due rows and, for each
//! one, resolves the recipient, renders the template, sends the
//! mail, and records the outcome:
//!
//! * success → `sent_at` set, the row is never claimed again;
//! * transient failure (relay down, database hiccup) → the row
//!   is rescheduled with exponential backoff
//!   (`base * 2^(n-1)` seconds, capped at one hour);
//! * permanent failure (unknown template, unresolvable
//!   recipient, bad address, render error) → the row is parked
//!   (`attempts = max_attempts`, `last_error` kept) and never
//!   claimed again.
//!
//! Recipient resolution (NFR-42): the outbox stores only
//! `to_hash` (sha256 of the address) and the payload carries
//! `user_id`. The drain loads the user, takes `email` — or
//! `pending_email` for the `email-change-verify` template,
//! whose mail goes to the NEW address — and verifies the
//! sha256 matches `to_hash` before sending. A mismatch parks
//! the row: the drain must never send to an address the queue
//! did not name. The raw address is never logged (only the
//! outbox id + template).
//!
//! Delivery is at-least-once: if the mail is sent but the
//! `mark_sent` write fails, the row is claimed again next pass
//! and sent again. The window is one failed write, and the
//! claim already spent an attempt, so a row can duplicate at
//! most `max_attempts` times — the alternative (marking sent
//! before the relay answers) would lose mail silently.

use crate::error::MailError;
use crate::smtp::{MailTransport, OutgoingMail};
use crate::templates::TemplateEngine;
use chrono::{DateTime, Duration, Utc};
use detag_core::checksum::sha256_hex;
use detag_core::config::Settings;
use detag_core::db::DbPool;
use detag_core::repos::email_outbox::{self, OutboxMail};
use detag_core::repos::users;
use serde_json::json;
use uuid::Uuid;

/// The template that goes to the pending (new) address, not the
/// address on record: the drain resolves its recipient from
/// `users.pending_email` instead of `users.email`.
pub const EMAIL_CHANGE_VERIFY_TEMPLATE: &str = "email-change-verify";

/// Backoff ceiling: no retry is ever scheduled more than an
/// hour out, whatever the attempt count.
pub const MAX_BACKOFF_SECS: u64 = 3600;

/// The product name the templates address the user with.
pub const APP_NAME: &str = "Detag";

/// Drain configuration (from `Settings`; see PF_MAILER_*).
#[derive(Debug, Clone)]
pub struct DrainConfig {
    /// `PF_MAILER_MAX_ATTEMPTS` — attempts per row before it
    /// stops being claimed (default 5).
    pub max_attempts: u32,
    /// `PF_MAILER_BACKOFF_BASE_SECS` — backoff base (default 60).
    pub backoff_base_secs: u64,
    /// `PF_MAILER_DRAIN_BATCH` — max rows one pass claims
    /// (default 25).
    pub batch: u32,
    /// `PF_MAILER_DRAIN_INTERVAL_SECS` — worker loop wake
    /// interval (default 30).
    pub interval_secs: u64,
    /// `PF_PUBLIC_BASE_URL` — the links in the templates are
    /// built on this.
    pub base_url: String,
    /// `PF_MAIL_FROM` — the sender of every mail.
    pub from: String,
}

impl DrainConfig {
    /// From loaded settings (P1-028).
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            max_attempts: settings.mailer_max_attempts,
            backoff_base_secs: settings.mailer_backoff_base_secs,
            batch: settings.mailer_drain_batch,
            interval_secs: settings.mailer_drain_interval_secs,
            base_url: settings.public_base_url.clone(),
            from: settings.mail_from.clone(),
        }
    }
}

/// What one drain pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DrainOutcome {
    /// Rows claimed this pass.
    pub claimed: usize,
    /// Rows sent and marked.
    pub sent: usize,
    /// Rows that failed transiently (rescheduled with backoff).
    pub transient_failed: usize,
    /// Rows parked (permanent failure — never claimed again).
    pub permanent_failed: usize,
}

/// Exponential backoff after the n-th failed attempt (1-based):
/// `base * 2^(n-1)` seconds, capped at [`MAX_BACKOFF_SECS`].
/// Pure, so the schedule is unit-testable without a database.
pub fn backoff_delay_secs(attempts: u32, base_secs: u64) -> u64 {
    base_secs
        .saturating_mul(2u64.saturating_pow(attempts.saturating_sub(1)))
        .min(MAX_BACKOFF_SECS)
}

/// What one row's send did (internal; the pass aggregates it).
enum SendOutcome {
    Sent,
    TransientFailed,
    PermanentFailed,
}

/// What recipient resolution found.
enum ResolveOutcome {
    /// The address to send to (verified against `to_hash`).
    Ok(String),
    /// The data is wrong — park the row, never retry.
    Permanent(String),
    /// The database hiccuped — retry with backoff.
    Transient(MailError),
}

/// Resolve an outbox row's recipient: load the user from the
/// payload's `user_id`, take `email` (or `pending_email` for
/// the email-change verification mail), and verify its sha256
/// equals the row's `to_hash`. The address itself is never
/// logged — only the row id + template, at the call site.
async fn resolve_recipient(pool: &DbPool, row: &OutboxMail) -> ResolveOutcome {
    let user_id_raw = match row.payload.get("user_id").and_then(|v| v.as_str()) {
        Some(raw) => raw,
        None => {
            return ResolveOutcome::Permanent("payload has no user_id".to_string());
        }
    };
    let user_id = match Uuid::parse_str(user_id_raw) {
        Ok(id) => id,
        Err(_) => {
            return ResolveOutcome::Permanent("payload user_id is not a UUID".to_string());
        }
    };
    let user = match users::by_id(pool, user_id).await {
        Ok(Some(user)) => user,
        Ok(None) => return ResolveOutcome::Permanent("user not found".to_string()),
        Err(err) => return ResolveOutcome::Transient(MailError::Db(err)),
    };
    let address = if row.template == EMAIL_CHANGE_VERIFY_TEMPLATE {
        match user.pending_email {
            Some(email) => email,
            None => {
                return ResolveOutcome::Permanent(
                    "email-change-verify with no pending email".to_string(),
                );
            }
        }
    } else {
        user.email
    };
    match row.to_hash.as_deref() {
        Some(hash) if sha256_hex(address.as_bytes()) == hash => ResolveOutcome::Ok(address),
        Some(_) => {
            ResolveOutcome::Permanent("to_hash does not match the resolved address".to_string())
        }
        None => ResolveOutcome::Permanent("outbox row has no to_hash".to_string()),
    }
}

/// Run one drain pass: claim the due rows and send each one.
/// A database error on the claim itself propagates — the
/// worker loop logs it and retries next pass; per-row failures
/// are recorded on the row and counted, never propagated.
pub async fn drain_once<T: MailTransport>(
    pool: &DbPool,
    transport: &T,
    engine: &TemplateEngine,
    config: &DrainConfig,
    now: DateTime<Utc>,
) -> Result<DrainOutcome, MailError> {
    let rows = email_outbox::claim_due(
        pool,
        i64::from(config.batch),
        config.max_attempts as i32,
        now,
    )
    .await?;
    let mut outcome = DrainOutcome {
        claimed: rows.len(),
        ..DrainOutcome::default()
    };
    for row in &rows {
        match send_one(pool, transport, engine, config, row, now).await {
            SendOutcome::Sent => outcome.sent += 1,
            SendOutcome::TransientFailed => outcome.transient_failed += 1,
            SendOutcome::PermanentFailed => outcome.permanent_failed += 1,
        }
    }
    Ok(outcome)
}

/// Send one claimed row: resolve → render → send → record.
async fn send_one<T: MailTransport>(
    pool: &DbPool,
    transport: &T,
    engine: &TemplateEngine,
    config: &DrainConfig,
    row: &OutboxMail,
    now: DateTime<Utc>,
) -> SendOutcome {
    let to = match resolve_recipient(pool, row).await {
        ResolveOutcome::Ok(to) => to,
        ResolveOutcome::Permanent(reason) => {
            return park(pool, row, config, &reason).await;
        }
        ResolveOutcome::Transient(err) => {
            return record_transient(pool, row, config, now, &err.to_string()).await;
        }
    };
    // The render context is the payload plus the drain's own
    // variables. A non-object payload cannot render — park it:
    // the payload is fixed, retrying cannot help.
    let mut payload = match row.payload.clone() {
        serde_json::Value::Object(map) => map,
        _ => {
            return park(pool, row, config, "payload is not a JSON object").await;
        }
    };
    payload.insert("base_url".to_string(), json!(config.base_url));
    payload.insert("app_name".to_string(), json!(APP_NAME));
    let rendered = match engine.render(&row.template, &serde_json::Value::Object(payload)) {
        Ok(rendered) => rendered,
        Err(err) => {
            // Render errors are permanent by construction
            // (unknown template, missing variable).
            debug_assert!(err.is_permanent(), "render errors must be permanent");
            return park(pool, row, config, &err.to_string()).await;
        }
    };
    let mail = OutgoingMail {
        to,
        from: config.from.clone(),
        subject: rendered.subject,
        text_body: rendered.text_body,
        html_body: rendered.html_body,
    };
    match transport.send(&mail).await {
        Ok(()) => {
            if let Err(err) = email_outbox::mark_sent(pool, row.id).await {
                // The mail went out but the mark failed: the row
                // will be claimed again and sent again
                // (at-least-once — see the module docs). Loud, so
                // ops sees the duplicate risk instead of silence.
                tracing::error!(
                    outbox_id = %row.id,
                    template = %row.template,
                    error = %err,
                    "mail sent but mark_sent failed — the row will be sent again next pass"
                );
                return SendOutcome::TransientFailed;
            }
            tracing::info!(
                outbox_id = %row.id,
                template = %row.template,
                "outbox mail sent"
            );
            SendOutcome::Sent
        }
        Err(err) => {
            if err.is_permanent() {
                park(pool, row, config, &err.to_string()).await
            } else {
                record_transient(pool, row, config, now, &err.to_string()).await
            }
        }
    }
}

/// Park a row: it can never be sent, so stop claiming it.
/// The reason lands in `last_error` for ops; a failure to
/// park is logged (the row stays claimable — the next pass
/// parks it again).
async fn park(pool: &DbPool, row: &OutboxMail, config: &DrainConfig, reason: &str) -> SendOutcome {
    tracing::warn!(
        outbox_id = %row.id,
        template = %row.template,
        reason,
        "outbox row parked (permanent failure)"
    );
    if let Err(err) =
        email_outbox::record_permanent_failure(pool, row.id, config.max_attempts as i32, reason)
            .await
    {
        tracing::error!(
            outbox_id = %row.id,
            error = %err,
            "failed to park outbox row"
        );
    }
    SendOutcome::PermanentFailed
}

/// Record a transient failure: reschedule the row with
/// exponential backoff from `now`. `row.attempts` is already
/// the post-claim count (the claim bumped it), so the n-th
/// failed attempt waits `base * 2^(n-1)`.
async fn record_transient(
    pool: &DbPool,
    row: &OutboxMail,
    config: &DrainConfig,
    now: DateTime<Utc>,
    reason: &str,
) -> SendOutcome {
    let delay = backoff_delay_secs(row.attempts as u32, config.backoff_base_secs);
    let next = now + Duration::seconds(delay as i64);
    tracing::warn!(
        outbox_id = %row.id,
        template = %row.template,
        attempts = row.attempts,
        delay_secs = delay,
        reason,
        "outbox mail failed, retry scheduled"
    );
    if let Err(err) = email_outbox::record_failure(pool, row.id, reason, next).await {
        tracing::error!(
            outbox_id = %row.id,
            error = %err,
            "failed to record outbox retry"
        );
    }
    SendOutcome::TransientFailed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_each_attempt_and_caps_at_one_hour() {
        assert_eq!(backoff_delay_secs(1, 60), 60, "1st failure waits base");
        assert_eq!(backoff_delay_secs(2, 60), 120);
        assert_eq!(backoff_delay_secs(3, 60), 240);
        assert_eq!(backoff_delay_secs(4, 60), 480);
        assert_eq!(backoff_delay_secs(5, 60), 960);
        assert_eq!(backoff_delay_secs(6, 60), 1920, "6th failure: 60 * 32");
        assert_eq!(
            backoff_delay_secs(7, 60),
            3600,
            "60 * 64 = 3840 caps at one hour"
        );
        assert_eq!(backoff_delay_secs(20, 60), 3600, "far out caps too");
        assert_eq!(backoff_delay_secs(1, 1), 1, "base 1 stays 1");
        assert_eq!(backoff_delay_secs(13, 1), 3600, "base 1 caps at 4096");
        assert_eq!(
            backoff_delay_secs(0, 60),
            60,
            "attempt 0 is base (defensive)"
        );
    }
}
