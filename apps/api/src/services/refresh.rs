//! Refresh rotation (P1-014): the 30-day opaque refresh token.
//!
//! A login mints one session row per login (P1-003): the
//! refresh token is 32 random bytes, stored as its sha256
//! only, with a `family_id` (one family per login). This
//! service rotates it:
//!
//! - **Rotate:** presenting a live refresh token mints a new
//!   access token (15 min) + a new refresh token in the SAME
//!   family, and marks the old row replaced (`revoked_at` +
//!   `replaced_by` = the new row's id). One transaction:
//!   the new session row + the replacement mark + the
//!   `session.refreshed` audit.
//! - **Reuse:** presenting an ALREADY-REPLACED token means
//!   the token was stolen (or presented twice at once): the
//!   whole family is revoked, a `session.refresh_reused`
//!   audit is written, and a security email is queued
//!   (`refresh-reuse-alert`). The answer is 401 `PF-1007`
//!   (`RefreshReuse`).
//! - **Dead token:** an unknown hash, a merely-revoked row
//!   (suspend, logout, family revocation), or an expired
//!   session is the generic 401 `PF-1001` — the hash lookup
//!   must not distinguish "never existed" from "dead".
//!
//! A rotated row carries BOTH marks (`revoked_at` +
//! `replaced_by`); a merely revoked row carries `revoked_at`
//! only. That difference is what distinguishes reuse
//! (PF-1007) from a dead token (PF-1001). The replacement
//! mark is conditional (`replaced_by IS NULL`): two
//! concurrent presents of the same token race, and the loser
//! runs the reuse path instead of minting a second live
//! session in the family.
//!
//! An impersonation token (P1-010) can never refresh: there
//! is no session row behind it, so the hash lookup fails
//! with the generic 401. A suspended user's token cannot
//! refresh either: the suspend revoked every session, so the
//! row is already dead (no separate suspended check is
//! needed here).

use crate::AppState;
use chrono::{Duration, Utc};
use detag_core::{
    checksum::sha256_hex,
    db::PoolConfig,
    jwt::{issue_access_token, ACCESS_TOKEN_TTL_SECS, REFRESH_TOKEN_TTL_SECS},
    repos::{
        audit_logs::{self, NewAuditLog},
        email_outbox::{self, NewOutboxMail},
        sessions::{self, NewSession, Session},
        users, RepoError,
    },
    tokens::generate_token,
    tx::{self, TxError},
};
use thiserror::Error;

use super::login::{DeviceInfo, LoginResponse};

/// The security-email template queued when a rotated refresh
/// token is reused (P1-014). The mailer does not exist yet —
/// the outbox row is the queue; the payload carries only the
/// user id (no token, no details).
pub const REFRESH_REUSE_ALERT_TEMPLATE: &str = "refresh-reuse-alert";

/// What a refresh can fail with.
#[derive(Debug, Error)]
pub enum RefreshError {
    /// Unknown, revoked, or expired refresh token — the
    /// generic unauthenticated error (no enumeration).
    #[error("invalid refresh token")]
    Invalid,
    /// A rotated token presented again — the family is
    /// revoked, the audit + security email are queued.
    #[error("refresh token reuse detected")]
    Reuse,
    #[error("refresh failed")]
    Internal,
}

/// What a successful refresh returns: the response body
/// (same shape as login) plus the raw new refresh token (the
/// handler sets it as the cookie). The raw token must never
/// be logged or stored — only its sha256 lives in the new
/// session row.
#[derive(Debug)]
pub struct RefreshOutcome {
    pub response: LoginResponse,
    pub refresh_token: String,
}

/// The transaction closure's error: small on purpose — it must
/// not recurse into [`RefreshError`].
#[derive(Debug)]
enum RefreshTxError {
    Db(RepoError),
    /// The old row was already replaced (a concurrent
    /// rotation won the race): the tx rolls back and the
    /// caller runs the reuse path instead.
    AlreadyRotated,
}

/// Rotate a refresh token (P1-014). `refresh_token` is the raw
/// token from the `detag_refresh` cookie; `device` is the
/// current request's device (user-agent + socket IP), which
/// becomes the new session row's device record.
pub async fn refresh_session(
    state: &AppState,
    refresh_token: &str,
    device: &DeviceInfo,
) -> Result<RefreshOutcome, RefreshError> {
    let hash = sha256_hex(refresh_token.as_bytes());
    let session = sessions::by_refresh_hash(&state.pool, &hash)
        .await
        .map_err(|err| {
            tracing::error!(error = ?err, "refresh: session lookup failed");
            RefreshError::Internal
        })?;
    let Some(session) = session else {
        // Unknown hash: no such session ever existed (or the
        // row is gone with its user). Generic 401, no detail.
        return Err(RefreshError::Invalid);
    };
    // Replaced first: a rotated row carries BOTH marks, so the
    // revoked check below must not swallow the reuse case.
    if session.replaced_by.is_some() {
        return reuse_family(state, &session).await;
    }
    if session.revoked_at.is_some() {
        // Merely revoked (suspend, logout, family revocation)
        // — dead, not reuse.
        return Err(RefreshError::Invalid);
    }
    if session.expires_at <= Utc::now() {
        return Err(RefreshError::Invalid);
    }
    let user = users::by_id(&state.pool, session.user_id)
        .await
        .map_err(|err| {
            tracing::error!(error = ?err, "refresh: user lookup failed");
            RefreshError::Internal
        })?;
    let Some(user) = user else {
        // The session row cascades with its user, so this is
        // unreachable in practice — a dead token either way.
        return Err(RefreshError::Invalid);
    };

    // Rotate, in one transaction: the new session row (same
    // family, new hash, this request's device, fresh 30-day
    // expiry) + the old row's replacement mark + the audit.
    // The new access token is issued inside the service (the
    // handler only renders it).
    let new_refresh_token = generate_token();
    let new_refresh_hash = sha256_hex(new_refresh_token.as_bytes());
    let new_access_token = issue_access_token(
        &state.settings.jwt_secret,
        user.id,
        &user.role,
        ACCESS_TOKEN_TTL_SECS,
    )
    .map_err(|err| {
        tracing::error!(error = ?err, "refresh: access token issue failed");
        RefreshError::Internal
    })?;
    let device_json = serde_json::json!({
        "ua": device.user_agent,
        "ip_hash": sha256_hex(device.ip.as_bytes()),
    });
    let expires_at = Utc::now() + Duration::seconds(REFRESH_TOKEN_TTL_SECS);
    let timeout = PoolConfig::default().transaction_timeout;
    let rotated = tx::with_tx(&state.pool, timeout, |conn| {
        let session = session.clone();
        let user = user.clone();
        let new_refresh_hash = new_refresh_hash.clone();
        let device_json = device_json.clone();
        Box::pin(async move {
            let new_session = sessions::create(
                &mut *conn,
                &NewSession {
                    user_id: user.id,
                    refresh_hash: new_refresh_hash,
                    family_id: session.family_id,
                    device: device_json,
                    expires_at,
                },
            )
            .await
            .map_err(RefreshTxError::Db)?;
            let marked = sessions::mark_replaced(&mut *conn, session.id, new_session.id)
                .await
                .map_err(RefreshTxError::Db)?;
            if !marked {
                // A concurrent rotation won the race: the
                // token was presented twice at once. Return
                // the marker so the tx rolls the new row back
                // and the caller runs the reuse path.
                return Err(RefreshTxError::AlreadyRotated);
            }
            audit_logs::insert(
                &mut *conn,
                &NewAuditLog {
                    actor_user_id: Some(user.id),
                    actor_client: None,
                    action: "session.refreshed".to_string(),
                    target_type: "user".to_string(),
                    target_id: Some(user.id.to_string()),
                    before: None,
                    after: Some(serde_json::json!({ "family_id": session.family_id })),
                    reason: None,
                },
            )
            .await
            .map_err(RefreshTxError::Db)?;
            Ok(new_session)
        })
    })
    .await;
    match rotated {
        Ok(_new_session) => {}
        Err(TxError::Inner(RefreshTxError::AlreadyRotated)) => {
            // The concurrent rotation won: run the reuse path
            // (revoke the family + audit + security mail) and
            // answer Reuse. The new row from the aborted tx is
            // gone (rolled back); the winner's row dies with
            // the family, as the spec requires for a reuse.
            return reuse_family(state, &session).await;
        }
        Err(TxError::Inner(RefreshTxError::Db(db_err))) => {
            tracing::error!(error = ?db_err, "refresh: rotation transaction failed (db)");
            return Err(RefreshError::Internal);
        }
        Err(other) => {
            tracing::error!(error = ?other, "refresh: rotation transaction failed");
            return Err(RefreshError::Internal);
        }
    }
    Ok(RefreshOutcome {
        response: LoginResponse {
            access_token: new_access_token,
            token_type: "Bearer".to_string(),
            expires_in: ACCESS_TOKEN_TTL_SECS,
        },
        refresh_token: new_refresh_token,
    })
}

/// The reuse path (P1-014): revoke the whole family, audit
/// `session.refresh_reused`, queue the security email — one
/// transaction, so they commit or roll back together. Always
/// answers `Reuse` (401 PF-1007). When the user is gone (the
/// session row cascades with its user, so unreachable in
/// practice), there is no family left to revoke and no
/// address to mail — the answer is still Reuse.
async fn reuse_family(state: &AppState, session: &Session) -> Result<RefreshOutcome, RefreshError> {
    let user = users::by_id(&state.pool, session.user_id)
        .await
        .map_err(|err| {
            tracing::error!(error = ?err, "refresh reuse: user lookup failed");
            RefreshError::Internal
        })?;
    if let Some(user) = user {
        let timeout = PoolConfig::default().transaction_timeout;
        tx::with_tx(&state.pool, timeout, |conn| {
            let session = session.clone();
            let user = user.clone();
            Box::pin(async move {
                let revoked = sessions::revoke_family(&mut *conn, session.family_id)
                    .await
                    .map_err(RefreshTxError::Db)?;
                audit_logs::insert(
                    &mut *conn,
                    &NewAuditLog {
                        actor_user_id: Some(user.id),
                        actor_client: None,
                        action: "session.refresh_reused".to_string(),
                        target_type: "user".to_string(),
                        target_id: Some(user.id.to_string()),
                        before: None,
                        after: Some(serde_json::json!({
                            "family_id": session.family_id,
                            "sessions_revoked": revoked,
                        })),
                        reason: None,
                    },
                )
                .await
                .map_err(RefreshTxError::Db)?;
                email_outbox::enqueue(
                    &mut *conn,
                    &NewOutboxMail {
                        to_hash: sha256_hex(user.email.as_bytes()),
                        template: REFRESH_REUSE_ALERT_TEMPLATE.to_string(),
                        payload: serde_json::json!({ "user_id": user.id }),
                        scheduled_at: None,
                    },
                )
                .await
                .map_err(RefreshTxError::Db)?;
                Ok(())
            })
        })
        .await
        .map_err(|err| match err {
            TxError::Inner(RefreshTxError::Db(db_err)) => {
                tracing::error!(error = ?db_err, "refresh reuse: transaction failed (db)");
                RefreshError::Internal
            }
            TxError::Inner(RefreshTxError::AlreadyRotated) => {
                // Unreachable: the reuse tx never marks a row
                // replaced.
                RefreshError::Internal
            }
            other => {
                tracing::error!(error = ?other, "refresh reuse: transaction failed");
                RefreshError::Internal
            }
        })?;
    }
    Err(RefreshError::Reuse)
}
