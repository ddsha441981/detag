//! Admin user actions (P1-008): suspend + restore.
//!
//! - [`suspend_user`] — status `suspended`, every live session
//!   revoked, every queued job held, `user.suspended` audited
//!   with the admin's reason. A suspend cancels any pending
//!   deletion (`deleted_at` is cleared — the grace does not
//!   survive admin intervention; the audit's `before` records
//!   the prior status + `deleted_at`).
//! - [`restore_user`] — status `active`, held jobs released
//!   back to `queued`, `user.restored` audited. Sessions stay
//!   revoked (revocation is permanent — the user logs in
//!   again); restore does NOT re-run a deletion.
//!
//! Both are idempotent: suspending an already-suspended
//! account (or restoring a non-suspended one) is a 200 no-op —
//! no write, no audit, no second mail. The caller is an admin
//! (the [`AdminContext`] extractor guarantees it); the target
//! is any user, including another admin (no hierarchy is
//! defined — suspending the last admin is an owner question,
//! not a code guard; see DECISIONS.md if it ever matters).
//!
//! What suspend does NOT do: it does not kill the access
//! token already issued — the token is stateless and stays
//! valid until it expires (≤ 15 min, the documented residual
//! window). It kills the refresh sessions (no new access
//! tokens) and blocks the next login (a suspended account's
//! login is 403 `PF-1003` after a correct password — see
//! `services/login.rs`). It does not hold a job already
//! running — only `queued` jobs move to `held`; the worker
//! finishes the in-flight one.

use crate::AppState;
use detag_core::{
    db::PoolConfig,
    repos::{
        audit_logs::{self, NewAuditLog},
        jobs, sessions,
        users::{self, User},
        RepoError,
    },
    tx::{self, TxError},
};
use detag_validator::{validate_reason, Validate, ValidationErrors};
use serde::Deserialize;
use thiserror::Error;
use utoipa::ToSchema;
use uuid::Uuid;

/// What suspend/restore can fail with.
#[derive(Debug, Error)]
pub enum AdminError {
    #[error("admin validation failed")]
    Validation(ValidationErrors),
    #[error("user not found")]
    NotFound,
    #[error("admin action failed")]
    Internal,
}

/// The transaction closure's error: small on purpose — it must
/// not recurse into [`AdminError`].
#[derive(Debug)]
enum AdminTxError {
    Db(RepoError),
}

/// The outcome of a suspend/restore: the updated user + what
/// the transition did. The counts are 0 on a no-op (`already`
/// is true) and 0 for the action that does not use them
/// (suspend never releases, restore never revokes).
#[derive(Debug)]
pub struct AdminActionOutcome {
    /// The user after the transition (or as it already was, on
    /// a no-op).
    pub user: User,
    /// The account was already in the target state — no write,
    /// no audit.
    pub already: bool,
    /// Sessions revoked (suspend only).
    pub sessions_revoked: u64,
    /// Queued jobs held (suspend only).
    pub jobs_held: u64,
    /// Held jobs released back to `queued` (restore only).
    pub jobs_released: u64,
}

/// POST /v1/admin/users/{id}/suspend body: the reason is
/// required (an admin action without one is not auditable).
#[derive(Debug, Deserialize, ToSchema)]
pub struct SuspendRequest {
    /// Why the account is suspended (1–500 characters after
    /// trimming; the audit carries it verbatim).
    pub reason: String,
}

impl Validate for SuspendRequest {
    fn validate(&self) -> Result<(), ValidationErrors> {
        let mut errors = ValidationErrors::default();
        if let Err(err) = validate_reason("reason", &self.reason) {
            errors.push(err);
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

/// Suspend a user (P1-008). One transaction: the status flip
/// (+ `deleted_at` cleared — the grace is cancelled) + the
/// session revocations + the job holds + the audit. The audit's
/// `after` carries the counts, so the row is self-describing.
pub async fn suspend_user(
    state: &AppState,
    admin: &User,
    target_id: Uuid,
    req: &SuspendRequest,
) -> Result<AdminActionOutcome, AdminError> {
    // Validate (again — the handler's ValidatedJson is the
    // gate; this call is the transform that yields the trimmed
    // reason to store).
    let reason = validate_reason("reason", &req.reason)
        .map_err(|err| AdminError::Validation(ValidationErrors(vec![err])))?;
    let target = users::by_id(&state.pool, target_id).await.map_err(|err| {
        tracing::error!(error = ?err, "admin suspend: user lookup failed");
        AdminError::Internal
    })?;
    let Some(target) = target else {
        return Err(AdminError::NotFound);
    };
    if target.status == "suspended" {
        return Ok(AdminActionOutcome {
            user: target,
            already: true,
            sessions_revoked: 0,
            jobs_held: 0,
            jobs_released: 0,
        });
    }
    let timeout = PoolConfig::default().transaction_timeout;
    let (user, sessions_revoked, jobs_held) = tx::with_tx(&state.pool, timeout, |conn| {
        let target = target.clone();
        let admin = admin.clone();
        let reason = reason.clone();
        Box::pin(async move {
            let user = users::suspend(&mut *conn, target.id)
                .await
                .map_err(AdminTxError::Db)?;
            let sessions_revoked = sessions::revoke_all_for_user(&mut *conn, target.id)
                .await
                .map_err(AdminTxError::Db)?;
            let jobs_held = jobs::hold_queued_for_user(&mut *conn, target.id)
                .await
                .map_err(AdminTxError::Db)?;
            audit_logs::insert(
                &mut *conn,
                &NewAuditLog {
                    actor_user_id: Some(admin.id),
                    actor_client: None,
                    action: "user.suspended".to_string(),
                    target_type: "user".to_string(),
                    target_id: Some(target.id.to_string()),
                    before: Some(serde_json::json!({
                        "status": target.status,
                        "deleted_at": target.deleted_at,
                    })),
                    after: Some(serde_json::json!({
                        "status": "suspended",
                        "sessions_revoked": sessions_revoked,
                        "jobs_held": jobs_held,
                    })),
                    reason: Some(reason),
                },
            )
            .await
            .map_err(AdminTxError::Db)?;
            Ok((user, sessions_revoked, jobs_held))
        })
    })
    .await
    .map_err(|err| match err {
        TxError::Inner(AdminTxError::Db(RepoError::NotFound)) => AdminError::NotFound,
        TxError::Inner(AdminTxError::Db(db_err)) => {
            tracing::error!(error = ?db_err, "admin suspend: transaction failed (db)");
            AdminError::Internal
        }
        other => {
            tracing::error!(error = ?other, "admin suspend: transaction failed");
            AdminError::Internal
        }
    })?;
    Ok(AdminActionOutcome {
        user,
        already: false,
        sessions_revoked,
        jobs_held,
        jobs_released: 0,
    })
}

/// Restore a suspended user (P1-008). One transaction: the
/// status flip (+ `deleted_at` cleared — a suspension cancels
/// any pending deletion) + the held jobs released + the audit.
/// Sessions stay revoked (the user logs in again). Restoring a
/// non-suspended account is a no-op.
pub async fn restore_user(
    state: &AppState,
    admin: &User,
    target_id: Uuid,
) -> Result<AdminActionOutcome, AdminError> {
    let target = users::by_id(&state.pool, target_id).await.map_err(|err| {
        tracing::error!(error = ?err, "admin restore: user lookup failed");
        AdminError::Internal
    })?;
    let Some(target) = target else {
        return Err(AdminError::NotFound);
    };
    if target.status != "suspended" {
        return Ok(AdminActionOutcome {
            user: target,
            already: true,
            sessions_revoked: 0,
            jobs_held: 0,
            jobs_released: 0,
        });
    }
    let timeout = PoolConfig::default().transaction_timeout;
    let (user, jobs_released) = tx::with_tx(&state.pool, timeout, |conn| {
        let target = target.clone();
        let admin = admin.clone();
        Box::pin(async move {
            let user = users::restore(&mut *conn, target.id)
                .await
                .map_err(AdminTxError::Db)?;
            let jobs_released = jobs::release_held_for_user(&mut *conn, target.id)
                .await
                .map_err(AdminTxError::Db)?;
            audit_logs::insert(
                &mut *conn,
                &NewAuditLog {
                    actor_user_id: Some(admin.id),
                    actor_client: None,
                    action: "user.restored".to_string(),
                    target_type: "user".to_string(),
                    target_id: Some(target.id.to_string()),
                    before: Some(serde_json::json!({ "status": target.status })),
                    after: Some(serde_json::json!({
                        "status": "active",
                        "jobs_released": jobs_released,
                    })),
                    reason: None,
                },
            )
            .await
            .map_err(AdminTxError::Db)?;
            Ok((user, jobs_released))
        })
    })
    .await
    .map_err(|err| match err {
        TxError::Inner(AdminTxError::Db(RepoError::NotFound)) => AdminError::NotFound,
        TxError::Inner(AdminTxError::Db(db_err)) => {
            tracing::error!(error = ?db_err, "admin restore: transaction failed (db)");
            AdminError::Internal
        }
        other => {
            tracing::error!(error = ?other, "admin restore: transaction failed");
            AdminError::Internal
        }
    })?;
    Ok(AdminActionOutcome {
        user,
        already: false,
        sessions_revoked: 0,
        jobs_held: 0,
        jobs_released,
    })
}
