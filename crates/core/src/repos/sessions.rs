//! Sessions repository (P1-003, E1.1). `sessions` is the
//! web-session table (migration 001, data-model doc): one row
//! per login, keyed by the sha256 of its refresh token.
//!
//! The refresh token itself is never stored — only its hash
//! (`refresh_hash`, unique). A login creates a row with a fresh
//! `family_id` (rotation lineage: P1-014's rotation replaces
//! the token inside the family and records the replacement in
//! `replaced_by`; reuse of a replaced token revokes the whole
//! family, PF-1007). `device` is `{ua, ip_hash}` — the
//! user-agent string and the sha256 of the client IP, so the
//! session list (P1-007) can show the device without storing
//! the raw IP.
//!
//! P1-003 needs only `create` (login). Lookup by hash,
//! revocation, and rotation land with P1-014/P1-017 — they are
//! not invented here.

use super::error::RepoError;
use chrono::{DateTime, Utc};
use serde_json::Value;
use uuid::Uuid;

/// A stored session row.
#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    pub id: Uuid,
    pub user_id: Uuid,
    /// sha256 of the refresh token (never the token itself).
    pub refresh_hash: String,
    /// Rotation lineage (one family per login; P1-014 rotates
    /// inside it).
    pub family_id: Uuid,
    /// `{ua, ip_hash}` — see the module docs.
    pub device: Value,
    pub expires_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub replaced_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

/// What a login inserts.
#[derive(Debug, Clone)]
pub struct NewSession {
    pub user_id: Uuid,
    pub refresh_hash: String,
    pub family_id: Uuid,
    pub device: Value,
    pub expires_at: DateTime<Utc>,
}

/// Insert a session row. The refresh token's hash must be
/// unique — a collision is a `RepoError::UniqueViolation`
/// (in practice impossible: the token is 32 random bytes).
pub async fn create(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    new: &NewSession,
) -> Result<Session, RepoError> {
    sqlx::query_as!(
        Session,
        r#"INSERT INTO sessions (user_id, refresh_hash, family_id, device, expires_at)
           VALUES ($1, $2, $3, $4, $5)
           RETURNING id, user_id, refresh_hash, family_id, device,
               expires_at, revoked_at, replaced_by, created_at"#,
        new.user_id,
        new.refresh_hash,
        new.family_id,
        new.device,
        new.expires_at,
    )
    .fetch_one(pool)
    .await
    .map_err(RepoError::from_sqlx)
}

/// List a user's sessions, newest first. The login test uses
/// it to prove the session row landed; P1-007 (`GET
/// /v1/me/sessions`) wires it into the handler.
pub async fn list_for_user(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    user_id: Uuid,
) -> Result<Vec<Session>, RepoError> {
    sqlx::query_as!(
        Session,
        r#"SELECT id, user_id, refresh_hash, family_id, device,
                  expires_at, revoked_at, replaced_by, created_at
           FROM sessions WHERE user_id = $1 ORDER BY created_at DESC, id DESC"#,
        user_id,
    )
    .fetch_all(pool)
    .await
    .map_err(RepoError::from_sqlx)
}
