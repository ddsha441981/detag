//! Abuse spike detection (P1-083, FR-P-07).
//!
//! Velocity rules: when a user creates at least
//! `jobs_per_minute` jobs inside the trailing `window_seconds`
//! window, the detector files one `abuse_reports` row (status
//! `open`) and writes one `abuse.spike_detected` audit row —
//! the audit rows + the queue rows ARE the alert (the mailer
//! precedent: `email.rate_limit_tripped`). A sustained spike
//! files once per window: the detector checks for an already
//! open report for the user inside the window first, and
//! re-checks inside the filing transaction, so the queue gets
//! one row to review, not one per job.
//!
//! The auto-suspend flag (`PF_ABUSE_AUTO_SUSPEND`, OFF by
//! default — the ticket's second AC) is the only enforcement:
//! when it is on, a spike also suspends the account through
//! the full P1-008 sequence (status flip + every live session
//! revoked + every queued job held, audited as
//! `user.suspended` with the velocity reason), in the same
//! transaction as the report. When it is off — the default —
//! a spike is reported and audited and nothing else happens:
//! the user keeps their account, their sessions, and their
//! jobs. A report is a review tripwire, not a block.
//!
//! # Invocation
//!
//! [`check_velocity`] is the invocable check, designed to be
//! called after each job insert — Phase 2's job-create calls
//! it once the 501 stub becomes a real intake (ASSUMPTION-044).
//! Until then there is no production caller: the tests seed
//! `jobs` rows directly. The check is cheap (one COUNT + one
//! indexed dedup scan) and safe to call per insert — the dedup
//! makes a sustained spike cost one report, not one per job.
//!
//! # What the threshold means
//!
//! The default 30 jobs/minute is a review tripwire, not a
//! quota: Free allows 10 images/day, Pro 500/day, so no
//! legitimate interactive burst reaches 30 creations in 60 s
//! — but a Business batch legitimately can, and that trip is
//! the expected signal (a report for a human to clear), not a
//! false positive to tune away. The threshold is operator
//! config, validated `>= 1` at boot.

use crate::audit::{record, NewAuditLog};
use crate::clock::Clock;
use crate::config::Settings;
use crate::db::{DbPool, PoolConfig};
use crate::repos::RepoError;
use crate::repos::{abuse_reports, jobs, sessions, users};
use crate::tx::{with_tx, TxError};
use chrono::{DateTime, Duration, Utc};
use std::sync::Arc;
use thiserror::Error;
use uuid::Uuid;

/// The audit action for a detected spike.
pub const SPIKE_AUDIT_ACTION: &str = "abuse.spike_detected";

/// The audit `actor_client` for detector writes (the spike
/// audit and, when auto-suspend is on, the suspension audit).
/// No user acts here — the detector is the actor.
pub const SPIKE_CLIENT: &str = "abuse-spike";

/// Default velocity threshold: 30 jobs in the window.
pub const DEFAULT_JOBS_PER_MINUTE: u32 = 30;

/// Default velocity window: 60 seconds.
pub const DEFAULT_WINDOW_SECONDS: u32 = 60;

/// What the velocity check can fail with (DB only — no Redis
/// leg: the `jobs` table is the source of truth for creations).
#[derive(Debug, Error)]
pub enum AbuseError {
    /// A repository call failed.
    #[error("abuse check failed")]
    Db(#[from] RepoError),
    /// The filing transaction failed to begin.
    #[error("abuse check transaction failed to begin: {0}")]
    TxBegin(sqlx::Error),
    /// The filing transaction failed to commit.
    #[error("abuse check transaction failed to commit: {0}")]
    TxCommit(sqlx::Error),
    /// A filing transaction was attempted inside another one
    /// (the `with_tx` nesting guard) — restructure the caller.
    #[error("abuse check nested transaction attempted")]
    TxNested,
}

impl From<TxError<RepoError>> for AbuseError {
    fn from(err: TxError<RepoError>) -> Self {
        match err {
            TxError::Inner(db_err) => Self::Db(db_err),
            TxError::Begin(sqlx_err) => Self::TxBegin(sqlx_err),
            TxError::Commit(sqlx_err) => Self::TxCommit(sqlx_err),
            TxError::Nested => Self::TxNested,
        }
    }
}

/// The velocity policy, from `Settings` (ADR-0047).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AbuseConfig {
    /// `PF_ABUSE_SPIKE_ENABLED` — the kill-switch. `false`
    /// means the check runs but files nothing (it still
    /// reports `checked: false`, like the dunning /
    /// reconciliation kill-switches).
    pub enabled: bool,
    /// `PF_ABUSE_SPIKE_JOBS_PER_MINUTE` — jobs created inside
    /// the window that trip the rule. Default 30, validated
    /// `>= 1` at boot.
    pub jobs_per_minute: u32,
    /// `PF_ABUSE_SPIKE_WINDOW_SECONDS` — the trailing window
    /// in seconds. Default 60, validated `>= 1` at boot.
    pub window_seconds: u32,
    /// `PF_ABUSE_AUTO_SUSPEND` — when true, a spike also
    /// suspends the account (the full P1-008 sequence).
    /// Default `false`: report + audit only, no enforcement.
    pub auto_suspend: bool,
}

impl AbuseConfig {
    /// The policy as configured (defaults: enabled, 30 / 60 s,
    /// auto-suspend off).
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            enabled: settings.abuse_spike_enabled,
            jobs_per_minute: settings.abuse_spike_jobs_per_minute,
            window_seconds: settings.abuse_spike_window_seconds,
            auto_suspend: settings.abuse_auto_suspend,
        }
    }
}

/// What one velocity check did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpikeCheck {
    /// Whether the check actually ran (`false` = disabled by
    /// config; every field below is the zero value then).
    pub checked: bool,
    /// Whether the count reached the threshold.
    pub detected: bool,
    /// Jobs the user created inside the window (0 when the
    /// check did not run).
    pub jobs_in_window: i64,
    /// The filed report's id (`None` when nothing was filed
    /// — under threshold, disabled, or a dedup hit returns
    /// the existing report's id instead).
    pub report_id: Option<Uuid>,
    /// Whether this check suspended the account (only ever
    /// true when `auto_suspend` is on and the account was not
    /// already suspended).
    pub suspended: bool,
    /// Whether the spike was already reported inside this
    /// window (a dedup hit — the report id is the existing
    /// row's, and nothing was written).
    pub already_reported: bool,
}

/// The pure velocity rule: the count trips the rule when it
/// reaches the threshold. A threshold of 0 would trip on any
/// count — callers pass a validated config (`>= 1` at boot),
/// so 0 never arrives here from production.
pub fn is_spike(jobs_in_window: i64, jobs_per_minute: u32) -> bool {
    jobs_in_window >= i64::from(jobs_per_minute)
}

/// The report's reason line: the velocity, the window, and
/// the threshold — everything a reviewer needs without
/// opening the audit row.
pub fn spike_reason(jobs_in_window: i64, window_seconds: u32, jobs_per_minute: u32) -> String {
    format!("velocity: {jobs_in_window} jobs in {window_seconds}s (threshold {jobs_per_minute})")
}

/// The report's notes: the same numbers in machine-readable
/// form (the audit row carries the structured JSON; the notes
/// are the plain-text copy for the queue view).
pub fn spike_notes(jobs_in_window: i64, window_seconds: u32, jobs_per_minute: u32) -> String {
    format!(
        "jobs_in_window={jobs_in_window}, window_seconds={window_seconds}, threshold={jobs_per_minute}"
    )
}

/// Check one user's creation velocity at `clock.now()`.
/// See the module docs for what a spike files and what
/// auto-suspend does. Under threshold (or disabled) nothing
/// is written — a quiet user leaves no trace at all.
pub async fn check_velocity(
    pool: &DbPool,
    clock: Arc<dyn Clock>,
    config: &AbuseConfig,
    user_id: Uuid,
) -> Result<SpikeCheck, AbuseError> {
    let now = clock.now();
    let mut check = SpikeCheck {
        checked: false,
        detected: false,
        jobs_in_window: 0,
        report_id: None,
        suspended: false,
        already_reported: false,
    };
    if !config.enabled {
        tracing::info!("abuse spike: disabled by PF_ABUSE_SPIKE_ENABLED — no-op");
        return Ok(check);
    }
    check.checked = true;
    let window_start = now - Duration::seconds(i64::from(config.window_seconds));
    let count = jobs::count_created_since(pool, user_id, window_start).await?;
    check.jobs_in_window = count;
    if !is_spike(count, config.jobs_per_minute) {
        return Ok(check);
    }
    check.detected = true;
    // Fast dedup outside the transaction: a sustained spike's
    // second check finds the first check's report and files
    // nothing. The transaction re-checks (below) — a
    // concurrent check may have filed between the count and
    // the filing, and that race must not double-file either.
    if let Some(existing) = abuse_reports::open_since(pool, user_id, window_start).await? {
        check.report_id = Some(existing.id);
        check.already_reported = true;
        return Ok(check);
    }
    let reason = spike_reason(count, config.window_seconds, config.jobs_per_minute);
    let notes = spike_notes(count, config.window_seconds, config.jobs_per_minute);
    let timeout = PoolConfig::default().transaction_timeout;
    let (report_id, suspended, already_reported) = with_tx(pool, timeout, |conn| {
        let config = *config;
        let reason = reason.clone();
        let notes = notes.clone();
        Box::pin(async move {
            // Re-check inside the transaction: the dedup scan
            // above raced the filing transaction.
            if let Some(existing) =
                abuse_reports::open_since(&mut *conn, user_id, window_start).await?
            {
                // The concurrent check filed first: this check
                // files nothing, and the report id is the
                // existing row's — the same dedup-hit outcome as
                // the fast path above, so `already_reported` is
                // true here too (the struct's contract).
                return Ok((existing.id, false, true));
            }
            let report_id =
                abuse_reports::insert_spike(&mut *conn, user_id, &reason, &notes).await?;
            let mut suspended = false;
            if config.auto_suspend {
                // The full P1-008 sequence (the admin suspend's
                // shape, actor none — the detector acts, not an
                // admin): status flip + sessions revoked + queued
                // jobs held, audited as `user.suspended`. An
                // already-suspended account is left alone — the
                // spike is still reported above, but a second
                // suspend would be a second audit row for a
                // no-op, and the P1-008 service is idempotent
                // for the same reason.
                if let Some(target) = users::by_id(&mut *conn, user_id).await? {
                    if target.status != "suspended" {
                        users::suspend(&mut *conn, user_id).await?;
                        let sessions_revoked =
                            sessions::revoke_all_for_user(&mut *conn, user_id).await?;
                        let jobs_held = jobs::hold_queued_for_user(&mut *conn, user_id).await?;
                        record(
                            &mut *conn,
                            &NewAuditLog {
                                actor_user_id: None,
                                actor_client: Some(SPIKE_CLIENT.to_string()),
                                action: "user.suspended".to_string(),
                                target_type: "user".to_string(),
                                target_id: Some(user_id.to_string()),
                                before: Some(serde_json::json!({
                                    "status": target.status,
                                    "deleted_at": target.deleted_at,
                                })),
                                after: Some(serde_json::json!({
                                    "status": "suspended",
                                    "sessions_revoked": sessions_revoked,
                                    "jobs_held": jobs_held,
                                })),
                                reason: Some(reason.clone()),
                            },
                        )
                        .await?;
                        suspended = true;
                    }
                }
            }
            // The spike audit lands last, so its `after` carries
            // the real suspension outcome — not the flag (an
            // already-suspended account reports `suspended:
            // false`: this check suspended nobody).
            record(
                &mut *conn,
                &NewAuditLog {
                    actor_user_id: None,
                    actor_client: Some(SPIKE_CLIENT.to_string()),
                    action: SPIKE_AUDIT_ACTION.to_string(),
                    target_type: "user".to_string(),
                    target_id: Some(user_id.to_string()),
                    before: Some(serde_json::json!({
                        "jobs_in_window": count,
                        "threshold": config.jobs_per_minute,
                        "window_seconds": config.window_seconds,
                    })),
                    after: Some(serde_json::json!({
                        "report_id": report_id,
                        "suspended": suspended,
                    })),
                    reason: Some(reason),
                },
            )
            .await?;
            Ok((report_id, suspended, false))
        })
    })
    .await
    .map_err(AbuseError::from)?;
    check.report_id = Some(report_id);
    check.suspended = suspended;
    check.already_reported = already_reported;
    tracing::info!(
        user_id = %user_id,
        jobs_in_window = count,
        threshold = config.jobs_per_minute,
        report_id = %report_id,
        suspended = suspended,
        "abuse spike: velocity rule tripped, report filed"
    );
    Ok(check)
}

/// The window start for a check at `now`: the trailing
/// `window_seconds` seconds. Split out for the unit tests —
/// the arithmetic is the one place a sign error would invert
/// the rule (a future start would count nothing, ever).
pub fn window_start(now: DateTime<Utc>, window_seconds: u32) -> DateTime<Utc> {
    now - Duration::seconds(i64::from(window_seconds))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn threshold_is_inclusive() {
        // At the threshold: a spike. One under: quiet.
        assert!(is_spike(30, 30));
        assert!(!is_spike(29, 30));
        assert!(is_spike(31, 30));
        // Zero jobs never trips a sane threshold.
        assert!(!is_spike(0, 30));
    }

    #[test]
    fn window_start_is_trailing() {
        let now = Utc::now();
        assert_eq!(window_start(now, 60), now - Duration::seconds(60));
        assert_eq!(window_start(now, 3600), now - Duration::seconds(3600));
    }

    #[test]
    fn reason_and_notes_carry_the_numbers() {
        assert_eq!(
            spike_reason(42, 60, 30),
            "velocity: 42 jobs in 60s (threshold 30)"
        );
        assert_eq!(
            spike_notes(42, 60, 30),
            "jobs_in_window=42, window_seconds=60, threshold=30"
        );
    }

    #[test]
    fn config_defaults_come_from_settings() {
        let settings = Settings::default();
        let config = AbuseConfig::from_settings(&settings);
        assert!(config.enabled);
        assert_eq!(config.jobs_per_minute, DEFAULT_JOBS_PER_MINUTE);
        assert_eq!(config.jobs_per_minute, 30);
        assert_eq!(config.window_seconds, DEFAULT_WINDOW_SECONDS);
        assert_eq!(config.window_seconds, 60);
        // The ticket's second AC: auto-suspend is OFF by default.
        assert!(!config.auto_suspend);
    }
}
