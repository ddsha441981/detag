//! Password-change session revocation (P1-018, FR-A-01):
//! the shared mechanism the password flows call when the
//! password changes — the reset flow (P1-025) and the future
//! change-password endpoint (no such route exists yet).
//!
//! [`revoke_sessions_on_password_change`] revokes every live
//! session of the user EXCEPT the current one, in one
//! transaction with the audit + the digest email, AND bumps the
//! revocation epoch (P1-021): `users.revoked_before = now()`,
//! so every access token issued before the password change dies
//! at the extractor (`iat < revoked_before`) — the session
//! revocation alone cannot kill a stateless token, and the
//! epoch is what closes that window. The bump is in the same
//! tx, so the epoch and the revoked sessions commit or roll
//! back together (they can never diverge: an epoch without the
//! revocations, or revocations without the epoch).
//! - `current_session_id = Some(id)`: the change-password
//!   case — the caller keeps the session the request came
//!   from (the session behind the presented refresh cookie,
//!   read the way the logout handler reads it) so the user
//!   is not signed out of the device they are holding. The
//!   KEPT session's refresh token still rotates — and the
//!   rotation mints fresh access tokens with a NEW `iat`
//!   (after the bump), so the user stays signed in while every
//!   pre-change token dies. A rotation in the same second as
//!   the bump is denied by the `<=` epoch comparison and
//!   self-heals on the next refresh (see
//!   `users::set_revoked_before`).
//! - `current_session_id = None`: the reset case — there is
//!   no current session (a reset happens logged-out), so
//!   every session is revoked.
//! - The audit is `session.revoked_on_password_change`
//!   (`after: {sessions_revoked, kept_session_id}` — the
//!   count is the sessions THIS call revoked; a session the
//!   rotation already revoked is not counted twice). No audit
//!   when nothing was revoked (P1-008's idempotent-no-op
//!   convention — the revocation is the audited action).
//! - The digest email is queued on EVERY call, even when
//!   nothing was revoked: the mail notifies about the
//!   PASSWORD CHANGE (with the digest of what was signed
//!   out), not about the revocation. Template
//!   `new-login-digest`, payload `{user_id,
//!   sessions_revoked}` — the mailer (P1-028/029) renders
//!   "your password changed; N other sessions were signed
//!   out" + the recent-login list from the session rows at
//!   render time (the rows are revoked, not deleted, so they
//!   are still there to read).
//!
//! Why the mechanism lands before the flows: the reset flow
//! (P1-025) is a later ticket, and no change-password route
//! exists yet — the same land-early pattern as P1-004's
//! lockout `reset` (the counter reset the reset flow will
//! call). The service is tested directly
//! (`tests/password_sessions.rs`); the flows wire it in
//! when they land.

use crate::AppState;
use detag_core::{
    checksum::sha256_hex,
    db::PoolConfig,
    repos::{
        audit_logs::{self, NewAuditLog},
        email_outbox::{self, NewOutboxMail},
        sessions::{self},
        users::{self},
        RepoError,
    },
    tx::{self, TxError},
};
use thiserror::Error;
use uuid::Uuid;

/// Outbox template for the password-change digest mail
/// (P1-018): sent to the address on record, payload
/// `{user_id, sessions_revoked}` — the mailer renders the
/// "your password changed" notice + the recent-login digest
/// from the session rows.
pub const NEW_LOGIN_DIGEST_TEMPLATE: &str = "new-login-digest";

/// What the revocation can fail with. `Internal` carries no
/// detail by design.
#[derive(Debug, Error)]
pub enum PasswordSessionsError {
    #[error("password-change revocation failed")]
    Internal,
}

/// The transaction closure's error: small on purpose — it must
/// not recurse into [`PasswordSessionsError`].
#[derive(Debug)]
enum PasswordSessionsTxError {
    Db(RepoError),
}

/// Revoke every live session of the user except
/// `current_session_id` (or every session when it is `None` —
/// the reset case), bump the revocation epoch (P1-021), audit,
/// and queue the digest mail — one transaction. Returns how
/// many sessions were revoked. The user is loaded first (for
/// the digest mail's recipient hash); a missing user is
/// [`PasswordSessionsError::Internal`] (the caller always
/// passes a user that exists — P1-025's reset flow loads it
/// before calling).
pub async fn revoke_sessions_on_password_change(
    state: &AppState,
    user_id: Uuid,
    current_session_id: Option<Uuid>,
) -> Result<u64, PasswordSessionsError> {
    let user = users::by_id(&state.pool, user_id).await.map_err(|err| {
        tracing::error!(error = ?err, "password sessions: user lookup failed");
        PasswordSessionsError::Internal
    })?;
    let Some(user) = user else {
        tracing::error!("password sessions: user {user_id} not found");
        return Err(PasswordSessionsError::Internal);
    };
    let timeout = PoolConfig::default().transaction_timeout;
    tx::with_tx(&state.pool, timeout, |conn| {
        let user = user.clone();
        Box::pin(async move {
            let revoked = match current_session_id {
                Some(keep) => sessions::revoke_all_except(&mut *conn, user.id, keep)
                    .await
                    .map_err(PasswordSessionsTxError::Db)?,
                None => sessions::revoke_all_for_user(&mut *conn, user.id)
                    .await
                    .map_err(PasswordSessionsTxError::Db)?,
            };
            // The epoch bump (P1-021): every access token issued
            // before now dies at the extractor. In the same tx
            // as the revocations, so the two can never diverge.
            users::set_revoked_before(&mut *conn, user.id)
                .await
                .map_err(PasswordSessionsTxError::Db)?;
            if revoked > 0 {
                audit_logs::insert(
                    &mut *conn,
                    &NewAuditLog {
                        actor_user_id: Some(user.id),
                        actor_client: None,
                        action: "session.revoked_on_password_change".to_string(),
                        target_type: "user".to_string(),
                        target_id: Some(user.id.to_string()),
                        before: None,
                        after: Some(serde_json::json!({
                            "sessions_revoked": revoked,
                            "kept_session_id": current_session_id,
                        })),
                        reason: None,
                    },
                )
                .await
                .map_err(PasswordSessionsTxError::Db)?;
            }
            email_outbox::enqueue(
                &mut *conn,
                &NewOutboxMail {
                    to_hash: sha256_hex(user.email.as_bytes()),
                    template: NEW_LOGIN_DIGEST_TEMPLATE.to_string(),
                    payload: serde_json::json!({
                        "user_id": user.id,
                        "sessions_revoked": revoked,
                    }),
                    scheduled_at: None,
                },
            )
            .await
            .map_err(PasswordSessionsTxError::Db)?;
            Ok(revoked)
        })
    })
    .await
    .map_err(|err| match err {
        TxError::Inner(PasswordSessionsTxError::Db(db_err)) => {
            tracing::error!(error = ?db_err, "password sessions: transaction failed (db)");
            PasswordSessionsError::Internal
        }
        other => {
            tracing::error!(error = ?other, "password sessions: transaction failed");
            PasswordSessionsError::Internal
        }
    })
}
