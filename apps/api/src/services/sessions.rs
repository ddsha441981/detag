//! Session listing + single revocation (P1-019, FR-A-01):
//! `GET /v1/me/sessions` and `DELETE /v1/me/sessions/{id}`.
//!
//! - [`list_sessions`] — the user's LIVE sessions (not
//!   revoked, not expired), newest first. Each item carries
//!   the parsed device label (`"Chrome on macOS"` —
//!   `user_agent::parse_user_agent`, a small heuristic table,
//!   never the raw UA string), the ip hash (a hash is not
//!   PII — it lets the user recognise "same network"), the
//!   creation time, the last-seen time (NULL for rows created
//!   before the P1-019 migration — no activity was recorded
//!   for them), and the expiry. There is NO "current" flag:
//!   the access token carries no session identity (P1-017's
//!   logout has the same limitation — only the refresh cookie
//!   identifies a session), so the handler cannot know which
//!   row the caller is. The user tells sessions apart by
//!   device + created + last_seen.
//! - [`revoke_session`] — revoke one of the user's sessions
//!   by id. Unknown id, or an id belonging to another user →
//!   [`SessionError::NotFound`] (404 `PF-2010` — no
//!   enumeration of other users' sessions). Already revoked
//!   → no-op success, no second audit (P1-008's convention).
//!   A live one is revoked + audited (`session.revoked`,
//!   `after: {session_id}`) in one transaction.

use crate::AppState;
use chrono::{DateTime, Utc};
use detag_core::{
    db::PoolConfig,
    repos::{
        audit_logs::{self, NewAuditLog},
        sessions::{self},
        users::User,
        RepoError,
    },
    tx::{self, TxError},
    user_agent::parse_user_agent,
};
use serde::Serialize;
use thiserror::Error;
use utoipa::ToSchema;
use uuid::Uuid;

/// One session in the listing.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SessionItem {
    /// The session id (the DELETE target).
    pub id: Uuid,
    /// The parsed device label, e.g. `"Chrome on macOS"`
    /// (never the raw User-Agent string).
    pub device: String,
    /// sha256 of the client IP at login (a hash, not PII) —
    /// `None` when the session row has no device record.
    pub ip_hash: Option<String>,
    /// When the session was created (the login that minted it).
    pub created_at: DateTime<Utc>,
    /// Last time the session was presented (the refresh flow
    /// advances it, throttled — P1-019). `None` for rows
    /// created before the migration (no activity recorded).
    pub last_seen: Option<DateTime<Utc>>,
    /// When the refresh token expires (30 days from login).
    pub expires_at: DateTime<Utc>,
}

impl SessionItem {
    /// Build the listing item from a session row. The device
    /// JSONB is `{ua, ip_hash}` — both read defensively (an
    /// old or malformed row yields `"Unknown device"` / `None`,
    /// never a panic).
    fn from_row(row: sessions::Session) -> Self {
        let ua = row
            .device
            .get("ua")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        let ip_hash = row
            .device
            .get("ip_hash")
            .and_then(|value| value.as_str())
            .map(str::to_string);
        Self {
            id: row.id,
            device: parse_user_agent(ua),
            ip_hash,
            created_at: row.created_at,
            last_seen: row.last_seen,
            expires_at: row.expires_at,
        }
    }
}

/// What listing / revoking a session can fail with.
/// `Internal` carries no detail by design.
#[derive(Debug, Error)]
pub enum SessionError {
    #[error("session not found")]
    NotFound,
    #[error("session operation failed")]
    Internal,
}

/// The transaction closure's error: small on purpose — it must
/// not recurse into [`SessionError`].
#[derive(Debug)]
enum SessionTxError {
    Db(RepoError),
}

/// List the user's live sessions, newest first.
pub async fn list_sessions(
    state: &AppState,
    user: &User,
) -> Result<Vec<SessionItem>, SessionError> {
    let rows = sessions::list_live_for_user(&state.pool, user.id, state.clock.now())
        .await
        .map_err(|err| {
            tracing::error!(error = ?err, "sessions: list failed");
            SessionError::Internal
        })?;
    Ok(rows.into_iter().map(SessionItem::from_row).collect())
}

/// Revoke one of the user's sessions by id. Returns `true`
/// when the session was revoked, `false` when it was already
/// revoked (a no-op success). `actor_user_id` is the audit
/// actor (P1-011): the impersonator when the caller is
/// impersonating, else `user.id`.
pub async fn revoke_session(
    state: &AppState,
    user: &User,
    actor_user_id: Uuid,
    session_id: Uuid,
) -> Result<bool, SessionError> {
    let session = sessions::by_id_for_user(&state.pool, session_id, user.id)
        .await
        .map_err(|err| {
            tracing::error!(error = ?err, "sessions: lookup failed");
            SessionError::Internal
        })?;
    let Some(session) = session else {
        return Err(SessionError::NotFound);
    };
    if session.revoked_at.is_some() {
        return Ok(false);
    }
    let timeout = PoolConfig::default().transaction_timeout;
    tx::with_tx(&state.pool, timeout, |conn| {
        let session = session.clone();
        let user = user.clone();
        Box::pin(async move {
            let revoked = sessions::revoke_one(&mut *conn, session.id)
                .await
                .map_err(SessionTxError::Db)?;
            if revoked {
                audit_logs::insert(
                    &mut *conn,
                    &NewAuditLog {
                        actor_user_id: Some(actor_user_id),
                        actor_client: None,
                        action: "session.revoked".to_string(),
                        target_type: "user".to_string(),
                        target_id: Some(user.id.to_string()),
                        before: None,
                        after: Some(serde_json::json!({ "session_id": session.id })),
                        reason: None,
                    },
                )
                .await
                .map_err(SessionTxError::Db)?;
            }
            Ok(revoked)
        })
    })
    .await
    .map_err(|err| match err {
        TxError::Inner(SessionTxError::Db(db_err)) => {
            tracing::error!(error = ?db_err, "sessions: revoke transaction failed (db)");
            SessionError::Internal
        }
        other => {
            tracing::error!(error = ?other, "sessions: revoke transaction failed");
            SessionError::Internal
        }
    })
}
