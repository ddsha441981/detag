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
//! Lookup by hash, rotation, and reuse landed with P1-014.
//! Revocation (`revoke_all_for_user`) landed early with
//! P1-008: suspend revokes every live session, and that
//! cannot wait for P1-014.

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

/// Revoke every live session of a user (P1-008 — the suspend
/// path; P1-014 still owns rotation/reuse, P1-017 logout).
/// Sets `revoked_at = now()` where it is still NULL and
/// returns how many sessions were revoked. A revoked refresh
/// token can no longer mint access tokens; the access token
/// already issued stays valid until it expires (≤ 15 min —
/// the documented residual window, see the api-spec login
/// paragraph). Revocation is permanent: restore does NOT
/// un-revoke — the user logs in again.
pub async fn revoke_all_for_user(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    user_id: Uuid,
) -> Result<u64, RepoError> {
    let result = sqlx::query!(
        "UPDATE sessions SET revoked_at = now() WHERE user_id = $1 AND revoked_at IS NULL",
        user_id,
    )
    .execute(pool)
    .await
    .map_err(RepoError::from_sqlx)?;
    Ok(result.rows_affected())
}

/// Look a session up by its refresh token's hash (P1-014 —
/// the refresh endpoint). `None` when no session carries the
/// hash — the caller treats that as an unknown token (401
/// `PF-1001`, the generic unauthenticated error; the hash
/// lookup must not distinguish "never existed" from "dead").
pub async fn by_refresh_hash(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    refresh_hash: &str,
) -> Result<Option<Session>, RepoError> {
    sqlx::query_as!(
        Session,
        r#"SELECT id, user_id, refresh_hash, family_id, device,
                  expires_at, revoked_at, replaced_by, created_at
           FROM sessions WHERE refresh_hash = $1"#,
        refresh_hash,
    )
    .fetch_optional(pool)
    .await
    .map_err(RepoError::from_sqlx)
}

/// Mark a session replaced by a new one (P1-014 — rotation).
/// Sets `revoked_at = now()` AND `replaced_by = new_id`, but
/// only where the row has not been replaced already
/// (`replaced_by IS NULL`). Returns `true` when the row was
/// marked, `false` when it was already replaced — the caller
/// treats that as a concurrent rotation (the token was
/// presented twice at once) and runs the reuse path instead
/// of minting a second live session in the family. A rotated
/// row carries BOTH marks; a merely revoked row (suspend,
/// logout, family revocation) carries `revoked_at` only —
/// that difference is what distinguishes reuse (PF-1007)
/// from a dead token (PF-1001).
pub async fn mark_replaced(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    old_id: Uuid,
    new_id: Uuid,
) -> Result<bool, RepoError> {
    let result = sqlx::query!(
        "UPDATE sessions SET revoked_at = now(), replaced_by = $2
         WHERE id = $1 AND replaced_by IS NULL",
        old_id,
        new_id,
    )
    .execute(pool)
    .await
    .map_err(RepoError::from_sqlx)?;
    Ok(result.rows_affected() > 0)
}

/// Revoke every live session in a family (P1-014 — the reuse
/// path). Sets `revoked_at = now()` where it is still NULL
/// and returns how many sessions were revoked. A row the
/// rotation already marked is not counted twice (its
/// `revoked_at` is already set).
pub async fn revoke_family(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    family_id: Uuid,
) -> Result<u64, RepoError> {
    let result = sqlx::query!(
        "UPDATE sessions SET revoked_at = now() WHERE family_id = $1 AND revoked_at IS NULL",
        family_id,
    )
    .execute(pool)
    .await
    .map_err(RepoError::from_sqlx)?;
    Ok(result.rows_affected())
}
