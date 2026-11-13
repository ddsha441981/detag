//! Logout service (P1-017, FR-A-01): revoke the presented
//! session, or every session.
//!
//! - [`logout_current`] — `POST /v1/auth/logout`: the
//!   refresh token in the `detag_refresh` cookie identifies
//!   the session to revoke (the access token carries no
//!   session identity — its `jti` is per-issuance, not linked
//!   to a session row — so a Bearer token alone cannot log
//!   one session out; see the api-spec logout paragraph).
//!   Unknown hash → [`LogoutError::Invalid`] (the generic
//!   401 `PF-1001` — the lookup must not say whether the
//!   session existed); an already-revoked session is a no-op
//!   success (no second audit — P1-008's idempotent-no-op
//!   convention). A revoked refresh token can no longer
//!   rotate (the refresh service answers PF-1001 for a
//!   revoked row — proven by `tests/logout.rs`); the access
//!   token already issued stays valid until it expires
//!   (≤ 15 min — the documented residual window).
//! - [`logout_all`] — `POST /v1/auth/logout-all`: revoke
//!   every session of the user (the handler identifies the
//!   user via the Bearer `AuthContext`). One transaction
//!   (revoke-all + the `session.logged_out_all` audit, with
//!   `after: {sessions_revoked}`); no audit when nothing was
//!   revoked (same no-op convention as the single logout).

use crate::AppState;
use detag_core::{
    checksum::sha256_hex,
    db::PoolConfig,
    repos::{
        audit_logs::{self, NewAuditLog},
        sessions::{self, Session},
        users::User,
        RepoError,
    },
    tx::{self, TxError},
};
use thiserror::Error;

/// What a logout can fail with. `Internal` carries no detail
/// by design; `Invalid` is the generic unauthenticated
/// answer (unknown or missing refresh token).
#[derive(Debug, Error)]
pub enum LogoutError {
    #[error("no live session for the presented refresh token")]
    Invalid,
    #[error("logout failed")]
    Internal,
}

/// The transaction closure's error: small on purpose — it must
/// not recurse into [`LogoutError`].
#[derive(Debug)]
enum LogoutTxError {
    Db(RepoError),
}

/// Revoke the session behind `refresh_token` (the raw token
/// from the cookie — only its sha256 is ever looked up).
/// Returns `true` when the session was revoked, `false` when
/// it was already revoked (a second logout of the same session
/// is a no-op success). An unknown hash is [`LogoutError::Invalid`].
pub async fn logout_current(state: &AppState, refresh_token: &str) -> Result<bool, LogoutError> {
    let session = sessions::by_refresh_hash(&state.pool, &sha256_hex(refresh_token.as_bytes()))
        .await
        .map_err(|err| {
            tracing::error!(error = ?err, "logout: session lookup failed");
            LogoutError::Internal
        })?;
    let Some(session) = session else {
        return Err(LogoutError::Invalid);
    };
    if session.revoked_at.is_some() {
        return Ok(false);
    }
    revoke_and_audit(state, &session).await
}

/// Revoke one session + audit it, one transaction.
async fn revoke_and_audit(state: &AppState, session: &Session) -> Result<bool, LogoutError> {
    let timeout = PoolConfig::default().transaction_timeout;
    tx::with_tx(&state.pool, timeout, |conn| {
        let session = session.clone();
        Box::pin(async move {
            let revoked = sessions::revoke_one(&mut *conn, session.id)
                .await
                .map_err(LogoutTxError::Db)?;
            if revoked {
                audit_logs::insert(
                    &mut *conn,
                    &NewAuditLog {
                        actor_user_id: Some(session.user_id),
                        actor_client: None,
                        action: "session.logged_out".to_string(),
                        target_type: "user".to_string(),
                        target_id: Some(session.user_id.to_string()),
                        before: None,
                        after: Some(serde_json::json!({ "session_id": session.id })),
                        reason: None,
                    },
                )
                .await
                .map_err(LogoutTxError::Db)?;
            }
            Ok(revoked)
        })
    })
    .await
    .map_err(|err| match err {
        TxError::Inner(LogoutTxError::Db(db_err)) => {
            tracing::error!(error = ?db_err, "logout: transaction failed (db)");
            LogoutError::Internal
        }
        other => {
            tracing::error!(error = ?other, "logout: transaction failed");
            LogoutError::Internal
        }
    })
}

/// Revoke every session of `user` (the caller, already loaded
/// by the extractor). Returns how many sessions were revoked.
/// `actor_user_id` is the audit actor (P1-011): the
/// impersonator when the caller is impersonating, else
/// `user.id`. No audit when nothing was revoked (the no-op
/// convention — the count is still returned, so the handler
/// can report it).
pub async fn logout_all(
    state: &AppState,
    user: &User,
    actor_user_id: uuid::Uuid,
) -> Result<u64, LogoutError> {
    let timeout = PoolConfig::default().transaction_timeout;
    tx::with_tx(&state.pool, timeout, |conn| {
        let user = user.clone();
        Box::pin(async move {
            let revoked = sessions::revoke_all_for_user(&mut *conn, user.id)
                .await
                .map_err(LogoutTxError::Db)?;
            if revoked > 0 {
                audit_logs::insert(
                    &mut *conn,
                    &NewAuditLog {
                        actor_user_id: Some(actor_user_id),
                        actor_client: None,
                        action: "session.logged_out_all".to_string(),
                        target_type: "user".to_string(),
                        target_id: Some(user.id.to_string()),
                        before: None,
                        after: Some(serde_json::json!({ "sessions_revoked": revoked })),
                        reason: None,
                    },
                )
                .await
                .map_err(LogoutTxError::Db)?;
            }
            Ok(revoked)
        })
    })
    .await
    .map_err(|err| match err {
        TxError::Inner(LogoutTxError::Db(db_err)) => {
            tracing::error!(error = ?db_err, "logout-all: transaction failed (db)");
            LogoutError::Internal
        }
        other => {
            tracing::error!(error = ?other, "logout-all: transaction failed");
            LogoutError::Internal
        }
    })
}
