//! Email-verification-token repository (P1-002, E1.1).
//! `email_verification_tokens` (migration 20261113014639):
//! one row per minted token, looked up by its sha256 hash.
//!
//! Single-use is enforced by the consume UPDATE, not by a
//! pre-read: `UPDATE ... SET used_at = now() WHERE token_hash
//! = $1 AND used_at IS NULL AND expires_at > now()` consumes
//! exactly one row, and a concurrent second verify finds zero
//! rows to consume. The service distinguishes unknown / used /
//! expired with a prior `by_hash` read (for the error code) and
//! relies on the UPDATE for the atomicity.

use super::error::RepoError;
use chrono::{DateTime, Utc};
use uuid::Uuid;

/// A stored verification-token row.
#[derive(Debug, Clone, PartialEq)]
pub struct VerificationToken {
    pub id: Uuid,
    pub user_id: Uuid,
    pub token_hash: String,
    pub expires_at: DateTime<Utc>,
    pub used_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// Mint a token row. The caller generated the raw token and
/// hashed it (`detag_core::tokens`); this stores the hash only.
/// Does NOT delete previous tokens — the service does that in
/// the same transaction (one live token per user).
pub async fn mint(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    user_id: Uuid,
    token_hash: &str,
    expires_at: DateTime<Utc>,
) -> Result<VerificationToken, RepoError> {
    sqlx::query_as!(
        VerificationToken,
        r#"INSERT INTO email_verification_tokens
               (user_id, token_hash, expires_at)
           VALUES ($1, $2, $3)
           RETURNING id, user_id, token_hash, expires_at, used_at, created_at"#,
        user_id,
        token_hash,
        expires_at,
    )
    .fetch_one(pool)
    .await
    .map_err(RepoError::from_sqlx)
}

/// Look up a token by its hash. `None` when no such token was
/// ever minted.
pub async fn by_hash(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    token_hash: &str,
) -> Result<Option<VerificationToken>, RepoError> {
    sqlx::query_as!(
        VerificationToken,
        r#"SELECT id, user_id, token_hash, expires_at, used_at, created_at
           FROM email_verification_tokens
           WHERE token_hash = $1"#,
        token_hash,
    )
    .fetch_optional(pool)
    .await
    .map_err(RepoError::from_sqlx)
}

/// Atomically consume a token: set `used_at` only when the
/// token is still unused and unexpired. Returns `true` when
/// this call consumed it; `false` when another verify already
/// did (or it expired in between) — the caller maps that to
/// `InvalidToken`, never to a distinct "already used" error.
pub async fn consume(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    token_hash: &str,
) -> Result<bool, RepoError> {
    let row = sqlx::query!(
        r#"UPDATE email_verification_tokens SET used_at = now()
           WHERE token_hash = $1 AND used_at IS NULL AND expires_at > now()
           RETURNING id"#,
        token_hash,
    )
    .fetch_optional(pool)
    .await
    .map_err(RepoError::from_sqlx)?;
    Ok(row.is_some())
}

/// Delete the user's previous UNUSED tokens (same tx as the
/// mint that replaces them). Used tokens stay — they are the
/// audit trail of consumed links; expired unused tokens are
/// pruned by the retention job (data-model doc).
pub async fn delete_unused_for_user(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    user_id: Uuid,
) -> Result<u64, RepoError> {
    let result = sqlx::query!(
        r#"DELETE FROM email_verification_tokens
           WHERE user_id = $1 AND used_at IS NULL"#,
        user_id,
    )
    .execute(pool)
    .await
    .map_err(RepoError::from_sqlx)?;
    Ok(result.rows_affected())
}
