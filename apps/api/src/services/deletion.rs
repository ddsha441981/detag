//! Account-deletion service (P1-007, FR-A-01): request +
//! cancel. The purge itself is the gc binary's job
//! (`apps/gc/src/purge.rs`) — this service owns only the
//! user-facing half.
//!
//! - [`request_account_deletion`] — the DELETE /v1/me handler
//!   calls it: one transaction (status + `deleted_at` +
//!   `account.delete_requested` audit + confirmation mail).
//!   Idempotent: a second request for an already-pending
//!   account is a no-op (no refreshed `deleted_at`, no second
//!   mail) — the grace runs from the FIRST request.
//! - [`cancel_deletion_in_tx`] — the login hook calls it
//!   inside the login session transaction: a login during the
//!   grace cancels the deletion (status active, `deleted_at`
//!   cleared, `account.delete_cancelled` audited). A concurrent
//!   cancel first is not an error — the goal state already
//!   holds, and the login proceeds.
//!
//! The confirmation mail (template `account-deletion`,
//! payload `{user_id}`) goes to the address on record: it is
//! the "your account will be deleted in 30 days" notice, and
//! the user who requested the deletion is the one who can
//! still cancel it by logging in.

use crate::AppState;
use detag_core::{
    checksum::sha256_hex,
    db::PoolConfig,
    repos::{
        audit_logs::{self, NewAuditLog},
        email_outbox::{self, NewOutboxMail},
        users::{self, User},
        RepoError,
    },
    tx::{self, TxError},
};
use sqlx::PgConnection;
use thiserror::Error;

/// Outbox template for the deletion-confirmation mail
/// (P1-007): sent to the address on record, payload
/// `{user_id}` — the mail drain renders the 30-day notice
/// from the user row.
pub const ACCOUNT_DELETION_TEMPLATE: &str = "account-deletion";

/// What requesting a deletion can fail with. `Internal`
/// carries no detail by design.
#[derive(Debug, Error)]
pub enum DeletionError {
    #[error("account deletion failed")]
    Internal,
}

/// The transaction closure's error: small on purpose — it must
/// not recurse into [`DeletionError`].
#[derive(Debug)]
enum DeletionTxError {
    Db(RepoError),
}

/// Request account deletion for `user` (already loaded by the
/// extractor — fresh per request). A second request while
/// already pending is a no-op 200-equivalent: no write, no
/// audit, no second mail.
pub async fn request_account_deletion(state: &AppState, user: &User) -> Result<(), DeletionError> {
    if user.status == "deletion_pending" {
        return Ok(());
    }
    let timeout = PoolConfig::default().transaction_timeout;
    tx::with_tx(&state.pool, timeout, |conn| {
        let user = user.clone();
        Box::pin(async move {
            let updated = users::request_deletion(&mut *conn, user.id)
                .await
                .map_err(DeletionTxError::Db)?;
            audit_logs::insert(
                &mut *conn,
                &NewAuditLog {
                    actor_user_id: Some(user.id),
                    actor_client: None,
                    action: "account.delete_requested".to_string(),
                    target_type: "user".to_string(),
                    target_id: Some(user.id.to_string()),
                    before: Some(serde_json::json!({ "status": user.status })),
                    after: Some(serde_json::json!({
                        "status": "deletion_pending",
                        "deleted_at": updated.deleted_at,
                    })),
                    reason: None,
                },
            )
            .await
            .map_err(DeletionTxError::Db)?;
            email_outbox::enqueue(
                &mut *conn,
                &NewOutboxMail {
                    to_hash: sha256_hex(user.email.as_bytes()),
                    template: ACCOUNT_DELETION_TEMPLATE.to_string(),
                    payload: serde_json::json!({ "user_id": user.id }),
                    scheduled_at: None,
                },
            )
            .await
            .map_err(DeletionTxError::Db)?;
            Ok(())
        })
    })
    .await
    .map_err(|err| match err {
        TxError::Inner(DeletionTxError::Db(db_err)) => {
            tracing::error!(error = ?db_err, "account deletion: transaction failed (db)");
            DeletionError::Internal
        }
        other => {
            tracing::error!(error = ?other, "account deletion: transaction failed");
            DeletionError::Internal
        }
    })?;
    Ok(())
}

/// Cancel a pending deletion inside the caller's transaction
/// (`conn`) — the login hook owns the tx (the cancel commits
/// with the new session). Returns `true` when this call
/// cancelled it, `false` when it was already cancelled (a
/// concurrent login got there first — not an error).
pub async fn cancel_deletion_in_tx(
    conn: &mut PgConnection,
    user: &User,
) -> Result<bool, RepoError> {
    let Some(_updated) = users::cancel_deletion(&mut *conn, user.id).await? else {
        return Ok(false);
    };
    audit_logs::insert(
        &mut *conn,
        &NewAuditLog {
            actor_user_id: Some(user.id),
            actor_client: None,
            action: "account.delete_cancelled".to_string(),
            target_type: "user".to_string(),
            target_id: Some(user.id.to_string()),
            before: Some(serde_json::json!({ "status": "deletion_pending" })),
            after: Some(serde_json::json!({ "status": "active" })),
            reason: None,
        },
    )
    .await?;
    Ok(true)
}
