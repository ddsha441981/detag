//! Enforcement workflow (P1-091, FR-C-02): the warning →
//! suspension → termination ladder, its template mails, and
//! the ladder policy as one pure function.
//!
//! # The ladder
//!
//! A violation leads, in order, to a warning, a suspension,
//! then a termination (FR-C-02). The rungs are the admin
//! endpoints `POST /v1/admin/users/{id}/warn`,
//! `/suspend` (P1-008) and `/terminate` (this ticket) —
//! one endpoint per rung, so the P1-046 audit table keeps
//! one unique action per write route (`user.warned` /
//! `user.suspended` / `user.terminated`).
//!
//! [`next_step`] is the policy in one place: from the user's
//! status and their warning count it names the next rung —
//! `active` with no warning → warn, `active` with at least
//! one warning → suspend (a repeat offender skips the
//! warning), `suspended` → terminate, `deletion_pending` →
//! nothing (terminal). The state is derived, not stored:
//! the status lives on the `users` row and the warnings are
//! the `user.warned` audit rows, so a suspension done
//! through the P1-008 endpoint or the P1-090 queue composes
//! with the ladder without a second table to keep in sync.
//! No new table, no new migration, no new config.
//!
//! # What each rung does (one transaction each)
//!
//! - [`warn_user`] — status unchanged; writes `user.warned`
//!   (actor the admin, reason the admin's) and enqueues the
//!   `enforcement-warning` mail. A warning is repeatable:
//!   each call is a new warning event, and the count is what
//!   moves the ladder.
//! - Suspension — the P1-008 sequence (status `suspended`,
//!   every live session revoked, every queued job held,
//!   `user.suspended` audited), plus the
//!   `enforcement-suspension` mail, enqueued in the same
//!   transaction by both suspension paths (the P1-008 admin
//!   endpoint and the P1-090 queue resolve — see
//!   [`enqueue_step_mail`]). A no-op suspend (already
//!   suspended) mails nothing: the user already got the mail
//!   for the suspension that put them there.
//! - [`terminate_user`] — status `deletion_pending` (the
//!   P1-007 grace: `deleted_at = now()`, purge after 30
//!   days), every live session revoked, every queued job
//!   held, `user.terminated` audited, and the
//!   `enforcement-termination` mail enqueued. Termination
//!   requires `suspended` — the ladder is sequential, an
//!   active account cannot be terminated outright (422
//!   `PF-2002`). Terminating an already-terminated account
//!   is a 200 no-op (the P1-007 idempotency).
//!
//! # Termination is sticky (the login half)
//!
//! A `deletion_pending` account's next login cancels the
//! deletion (P1-007) — for a self-requested deletion that
//! is the escape hatch, but for an admin termination it
//! would let the user un-terminate themselves by signing
//! in. [`terminated_by_admin`] tells the two apart from
//! the audit trail (the latest of `user.terminated` /
//! `account.delete_requested` for the user), and the login
//! service refuses a terminated account with 403 `PF-1004`
//! (`login.failure`, reason `terminated`) instead of
//! cancelling. A self-requested deletion still cancels on
//! login, exactly as before.
//!
//! # What termination does NOT do
//!
//! It does not cancel the user's subscriptions (billing
//! cancellation is the P1-071 support flow, out of scope
//! here — the subscription keeps billing until someone
//! cancels it), and it does not mail on the P1-083
//! auto-suspend path (`abuse.rs` calls `users::suspend`
//! directly; that path is off by default and has no mail
//! hook — a follow-up, not this ticket).

use crate::{services::register::UserResponse, AppState};
use detag_core::{
    audit::{record, NewAuditLog},
    checksum::sha256_hex,
    db::PoolConfig,
    repos::{
        audit_logs,
        email_outbox::{self, NewOutboxMail},
        jobs, sessions,
        users::{self, User},
        RepoError,
    },
    tx::{self, TxError},
};
use detag_validator::{validate_reason, ValidationErrors};
use serde::Serialize;
use sqlx::PgConnection;
use thiserror::Error;
use utoipa::ToSchema;
use uuid::Uuid;

/// The audit action for a warning (the matrix already
/// promised this name for the warn route).
pub const USER_WARNED_AUDIT_ACTION: &str = "user.warned";

/// The audit action for a termination (the matrix already
/// promised this name for the terminate route).
pub const USER_TERMINATED_AUDIT_ACTION: &str = "user.terminated";

/// The warning mail template.
pub const ENFORCEMENT_WARNING_TEMPLATE: &str = "enforcement-warning";
/// The suspension mail template.
pub const ENFORCEMENT_SUSPENSION_TEMPLATE: &str = "enforcement-suspension";
/// The termination mail template.
pub const ENFORCEMENT_TERMINATION_TEMPLATE: &str = "enforcement-termination";

/// One rung of the ladder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum EnforcementStep {
    /// First rung: a formal warning, no status change.
    Warn,
    /// Second rung: suspension (P1-008).
    Suspend,
    /// Third rung: termination (the P1-007 grace).
    Terminate,
}

/// The next rung for a user, from their status and their
/// warning count — the ladder policy in one pure function.
/// `active` with no warning → warn; `active` with at least
/// one warning → suspend; `suspended` → terminate;
/// `deletion_pending` (or anything unrecognised) → nothing.
pub fn next_step(status: &str, warnings: u64) -> Option<EnforcementStep> {
    match status {
        "active" => {
            if warnings == 0 {
                Some(EnforcementStep::Warn)
            } else {
                Some(EnforcementStep::Suspend)
            }
        }
        "suspended" => Some(EnforcementStep::Terminate),
        _ => None,
    }
}

/// What warn/terminate can fail with.
#[derive(Debug, Error)]
pub enum EnforcementError {
    #[error("enforcement validation failed")]
    Validation(ValidationErrors),
    #[error("user not found")]
    NotFound,
    /// The account is on the wrong rung for this step
    /// (warn on a suspended account, terminate on an
    /// active one) — 422 `PF-2002`.
    #[error("account is on the wrong rung for this step")]
    InvalidStatus,
    #[error("enforcement failed")]
    Internal,
}

/// The transaction closure's error: small on purpose — it
/// must not recurse into [`EnforcementError`].
#[derive(Debug)]
enum EnforcementTxError {
    Db(RepoError),
}

/// The outcome of a warn/terminate: the updated user + what
/// the transition did. The counts are 0 on a no-op
/// (`already` is true) and 0 for the step that does not use
/// them (warn never revokes or holds).
#[derive(Debug)]
pub struct EnforcementOutcome {
    /// The user after the transition (or as it already was,
    /// on a no-op).
    pub user: User,
    /// Which rung ran.
    pub step: EnforcementStep,
    /// The account was already in the target state — no
    /// write, no audit, no mail.
    pub already: bool,
    /// Sessions revoked (terminate only).
    pub sessions_revoked: u64,
    /// Queued jobs held (terminate only).
    pub jobs_held: u64,
}

/// The HTTP shape for a warn/terminate: the user + the rung
/// that ran + what it did.
#[derive(Debug, Serialize, ToSchema)]
pub struct EnforcementResponse {
    /// The user after the transition.
    pub user: UserResponse,
    /// Which rung ran (`warn` | `terminate`).
    pub step: EnforcementStep,
    /// The account was already in the target state.
    pub already: bool,
    /// Sessions revoked (terminate only).
    pub sessions_revoked: u64,
    /// Queued jobs held (terminate only).
    pub jobs_held: u64,
}

impl From<EnforcementOutcome> for EnforcementResponse {
    fn from(outcome: EnforcementOutcome) -> Self {
        Self {
            user: UserResponse::from(outcome.user),
            step: outcome.step,
            already: outcome.already,
            sessions_revoked: outcome.sessions_revoked,
            jobs_held: outcome.jobs_held,
        }
    }
}

/// How many warnings a user has: the `user.warned` audit
/// rows targeting them. The audit trail is the ladder's
/// memory — no second table.
pub async fn warnings_count(
    pool: &detag_core::db::DbPool,
    user_id: Uuid,
) -> Result<u64, EnforcementError> {
    let rows = audit_logs::list_for_target(pool, "user", &user_id.to_string())
        .await
        .map_err(|err| {
            tracing::error!(error = ?err, "enforcement: warning count read failed");
            EnforcementError::Internal
        })?;
    Ok(rows
        .iter()
        .filter(|row| row.action == USER_WARNED_AUDIT_ACTION)
        .count() as u64)
}

/// Was this pending deletion started by an admin
/// termination, or requested by the user themselves? The
/// latest of the two deletion-lifecycle actions for the
/// user decides: `user.terminated` after
/// `account.delete_requested` means the admin got there
/// last (a terminated user cannot log in to cancel, so no
/// `account.delete_cancelled` can follow a termination).
/// Both rows are written in the same transaction as the
/// status flip, so their `created_at` is the DB clock at
/// the flip — no wall-clock comparison is needed.
pub async fn terminated_by_admin(
    pool: &detag_core::db::DbPool,
    user: &User,
) -> Result<bool, EnforcementError> {
    let rows = audit_logs::list_for_target(pool, "user", &user.id.to_string())
        .await
        .map_err(|err| {
            tracing::error!(error = ?err, "enforcement: termination check failed");
            EnforcementError::Internal
        })?;
    let latest = rows
        .iter()
        .filter(|row| {
            row.action == USER_TERMINATED_AUDIT_ACTION || row.action == "account.delete_requested"
        })
        .max_by_key(|row| row.created_at);
    Ok(matches!(latest, Some(row) if row.action == USER_TERMINATED_AUDIT_ACTION))
}

/// Enqueue one ladder mail for `user` inside the caller's
/// transaction: the drain resolves the recipient from
/// `payload.user_id` and verifies `to_hash` against the
/// address on record (the account-deletion precedent), so
/// the payload carries the reason and nothing else
/// identifying. Shared by the warn/terminate steps here
/// and by both suspension paths (the P1-008 admin suspend
/// and the P1-090 queue suspend) — one enqueue shape for
/// every rung.
pub async fn enqueue_step_mail(
    conn: &mut PgConnection,
    user: &User,
    template: &str,
    reason: &str,
) -> Result<(), RepoError> {
    email_outbox::enqueue(
        conn,
        &NewOutboxMail {
            to_hash: sha256_hex(user.email.as_bytes()),
            template: template.to_string(),
            payload: serde_json::json!({ "user_id": user.id, "reason": reason }),
            scheduled_at: None,
        },
    )
    .await?;
    Ok(())
}

fn map_tx_error(context: &'static str) -> impl Fn(TxError<EnforcementTxError>) -> EnforcementError {
    move |err| match err {
        TxError::Inner(EnforcementTxError::Db(RepoError::NotFound)) => EnforcementError::NotFound,
        TxError::Inner(EnforcementTxError::Db(db_err)) => {
            tracing::error!(error = ?db_err, "{context}: transaction failed (db)");
            EnforcementError::Internal
        }
        other => {
            tracing::error!(error = ?other, "{context}: transaction failed");
            EnforcementError::Internal
        }
    }
}

/// Warn a user (P1-091). One transaction: the `user.warned`
/// audit row + the `enforcement-warning` mail. The status
/// does not change. Requires an `active` account — warning
/// a suspended or deletion-pending one is 422 (the ladder
/// only moves forward).
pub async fn warn_user(
    state: &AppState,
    admin: &User,
    target_id: Uuid,
    req: &crate::services::admin::ReasonRequest,
) -> Result<EnforcementOutcome, EnforcementError> {
    let reason = validate_reason("reason", &req.reason)
        .map_err(|err| EnforcementError::Validation(ValidationErrors(vec![err])))?;
    let target = users::by_id(&state.pool, target_id).await.map_err(|err| {
        tracing::error!(error = ?err, "enforcement warn: user lookup failed");
        EnforcementError::Internal
    })?;
    let Some(target) = target else {
        return Err(EnforcementError::NotFound);
    };
    if target.status != "active" {
        return Err(EnforcementError::InvalidStatus);
    }
    let timeout = PoolConfig::default().transaction_timeout;
    let user = tx::with_tx(&state.pool, timeout, |conn| {
        let admin = admin.clone();
        let reason = reason.clone();
        let target = target.clone();
        Box::pin(async move {
            let warnings_before =
                audit_logs::list_for_target(&mut *conn, "user", &target.id.to_string())
                    .await
                    .map_err(EnforcementTxError::Db)?
                    .iter()
                    .filter(|row| row.action == USER_WARNED_AUDIT_ACTION)
                    .count() as u64;
            record(
                &mut *conn,
                &NewAuditLog {
                    actor_user_id: Some(admin.id),
                    actor_client: None,
                    action: USER_WARNED_AUDIT_ACTION.to_string(),
                    target_type: "user".to_string(),
                    target_id: Some(target.id.to_string()),
                    before: Some(serde_json::json!({
                        "status": target.status,
                        "warnings": warnings_before,
                    })),
                    after: Some(serde_json::json!({
                        "status": target.status,
                        "warnings": warnings_before + 1,
                    })),
                    reason: Some(reason.clone()),
                },
            )
            .await
            .map_err(EnforcementTxError::Db)?;
            enqueue_step_mail(&mut *conn, &target, ENFORCEMENT_WARNING_TEMPLATE, &reason)
                .await
                .map_err(EnforcementTxError::Db)?;
            Ok(target)
        })
    })
    .await
    .map_err(map_tx_error("enforcement warn"))?;
    Ok(EnforcementOutcome {
        user,
        step: EnforcementStep::Warn,
        already: false,
        sessions_revoked: 0,
        jobs_held: 0,
    })
}

/// Terminate a user (P1-091). One transaction: the P1-007
/// deletion request (`status = 'deletion_pending'`,
/// `deleted_at = now()` — the 30-day grace) + every live
/// session revoked + every queued job held + the
/// `user.terminated` audit row + the
/// `enforcement-termination` mail. Requires `suspended` —
/// the ladder is sequential, an active account cannot be
/// terminated outright (422). An already-terminated
/// account is a 200 no-op: no write, no audit, no mail.
pub async fn terminate_user(
    state: &AppState,
    admin: &User,
    target_id: Uuid,
    req: &crate::services::admin::ReasonRequest,
) -> Result<EnforcementOutcome, EnforcementError> {
    let reason = validate_reason("reason", &req.reason)
        .map_err(|err| EnforcementError::Validation(ValidationErrors(vec![err])))?;
    let target = users::by_id(&state.pool, target_id).await.map_err(|err| {
        tracing::error!(error = ?err, "enforcement terminate: user lookup failed");
        EnforcementError::Internal
    })?;
    let Some(target) = target else {
        return Err(EnforcementError::NotFound);
    };
    if target.status == "deletion_pending" {
        return Ok(EnforcementOutcome {
            user: target,
            step: EnforcementStep::Terminate,
            already: true,
            sessions_revoked: 0,
            jobs_held: 0,
        });
    }
    if target.status != "suspended" {
        return Err(EnforcementError::InvalidStatus);
    }
    let timeout = PoolConfig::default().transaction_timeout;
    let (user, sessions_revoked, jobs_held) = tx::with_tx(&state.pool, timeout, |conn| {
        let admin = admin.clone();
        let reason = reason.clone();
        let target = target.clone();
        Box::pin(async move {
            let user = users::request_deletion(&mut *conn, target.id)
                .await
                .map_err(EnforcementTxError::Db)?;
            let sessions_revoked = sessions::revoke_all_for_user(&mut *conn, target.id)
                .await
                .map_err(EnforcementTxError::Db)?;
            let jobs_held = jobs::hold_queued_for_user(&mut *conn, target.id)
                .await
                .map_err(EnforcementTxError::Db)?;
            record(
                &mut *conn,
                &NewAuditLog {
                    actor_user_id: Some(admin.id),
                    actor_client: None,
                    action: USER_TERMINATED_AUDIT_ACTION.to_string(),
                    target_type: "user".to_string(),
                    target_id: Some(target.id.to_string()),
                    before: Some(serde_json::json!({ "status": target.status })),
                    after: Some(serde_json::json!({
                        "status": "deletion_pending",
                        "deleted_at": user.deleted_at,
                        "sessions_revoked": sessions_revoked,
                        "jobs_held": jobs_held,
                    })),
                    reason: Some(reason.clone()),
                },
            )
            .await
            .map_err(EnforcementTxError::Db)?;
            enqueue_step_mail(
                &mut *conn,
                &target,
                ENFORCEMENT_TERMINATION_TEMPLATE,
                &reason,
            )
            .await
            .map_err(EnforcementTxError::Db)?;
            Ok((user, sessions_revoked, jobs_held))
        })
    })
    .await
    .map_err(map_tx_error("enforcement terminate"))?;
    Ok(EnforcementOutcome {
        user,
        step: EnforcementStep::Terminate,
        already: false,
        sessions_revoked,
        jobs_held,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_step_walks_the_ladder() {
        // A fresh account warns first.
        assert_eq!(next_step("active", 0), Some(EnforcementStep::Warn));
        // A warned account suspends next — the repeat
        // offender skips the warning.
        assert_eq!(next_step("active", 1), Some(EnforcementStep::Suspend));
        assert_eq!(next_step("active", 3), Some(EnforcementStep::Suspend));
        // A suspended account terminates next.
        assert_eq!(next_step("suspended", 1), Some(EnforcementStep::Terminate));
        // A terminated account has no next rung.
        assert_eq!(next_step("deletion_pending", 1), None);
        // An unrecognised status names nothing — the
        // caller treats it as "no ladder applies".
        assert_eq!(next_step("bogus", 0), None);
    }
}
