//! Email-verification-token repository (P1-002, E1.1; purpose
//! column P1-006).
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
//!
//! Purpose (P1-006, migration 20261113050515): a token is either
//! `verification` (register/resend — consumed by verify-email)
//! or `email_change` (consumed by confirm-email-change). The two
//! purposes are never interchangeable: the service checks the
//! purpose on the fetched row and answers `InvalidToken` for a
//! token of the wrong purpose. Minting still deletes ALL the
//! user's previous unused tokens, whatever their purpose (one
//! live token per user).

use super::error::RepoError;
use chrono::{DateTime, Utc};
use uuid::Uuid;

/// A stored verification-token row.
#[derive(Debug, Clone, PartialEq)]
pub struct VerificationToken {
    pub id: Uuid,
    pub user_id: Uuid,
    pub token_hash: String,
    /// `verification` or `email_change` (P1-006) — the two are
    /// never interchangeable.
    pub purpose: String,
    pub expires_at: DateTime<Utc>,
    pub used_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// The three token purposes (the migration CHECK admits nothing
/// else; these constants keep the strings in one place).
pub const PURPOSE_VERIFICATION: &str = "verification";
pub const PURPOSE_EMAIL_CHANGE: &str = "email_change";
pub const PURPOSE_PASSWORD_RESET: &str = "password_reset";

/// Mint a token row. The caller generated the raw token and
/// hashed it (`detag_core::tokens`); this stores the hash only.
/// Does NOT delete previous tokens — the service does that in
/// the same transaction (one live token per user).
pub async fn mint(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    user_id: Uuid,
    token_hash: &str,
    purpose: &str,
    expires_at: DateTime<Utc>,
) -> Result<VerificationToken, RepoError> {
    sqlx::query_as!(
        VerificationToken,
        r#"INSERT INTO email_verification_tokens
               (user_id, token_hash, purpose, expires_at)
           VALUES ($1, $2, $3, $4)
           RETURNING id, user_id, token_hash, purpose, expires_at, used_at, created_at"#,
        user_id,
        token_hash,
        purpose,
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
        r#"SELECT id, user_id, token_hash, purpose, expires_at, used_at, created_at
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

/// Delete the user's previous UNUSED tokens of one purpose
/// (same tx as the operation that supersedes them). P1-006 uses
/// it when a pending email change is cleared: the live change
/// token dies with the intent, while a registration token (if
/// any) survives — clearing an intent must not force the user
/// to re-verify their current address.
pub async fn delete_unused_for_purpose(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    user_id: Uuid,
    purpose: &str,
) -> Result<u64, RepoError> {
    let result = sqlx::query!(
        r#"DELETE FROM email_verification_tokens
           WHERE user_id = $1 AND used_at IS NULL AND purpose = $2"#,
        user_id,
        purpose,
    )
    .execute(pool)
    .await
    .map_err(RepoError::from_sqlx)?;
    Ok(result.rows_affected())
}
