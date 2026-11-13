//! Users repository (P0-041, E0.4). SQL lives here, not in services
//! (coding standards). `query_as!` macros are compile-time checked
//! against the live schema; offline data is checked in under
//! `.sqlx/` (see `just sqlx-prepare`).

use super::error::RepoError;
use chrono::{DateTime, Utc};
use uuid::Uuid;

/// A row of `users` (migration 001). `settings` is the raw jsonb
/// value; services interpret its keys. `display_name` /
/// `pending_email` are the P1-005 profile fields (migration
/// 20261113040605): the display name (nullable until set) and
/// the email-change intent (nullable until a change is
/// requested; P1-006 verifies + swaps it).
#[derive(Debug, Clone, PartialEq)]
pub struct User {
    pub id: Uuid,
    pub email: String,
    pub password_hash: String,
    pub email_verified_at: Option<DateTime<Utc>>,
    pub role: String,
    pub status: String,
    pub tos_version: Option<String>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub display_name: Option<String>,
    pub pending_email: Option<String>,
    pub settings: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Insert a user. Duplicate email (case-insensitive — the column is
/// citext) fails with `RepoError::UniqueViolation` naming
/// `users_email_key`; a bad role/status fails with `CheckViolation`.
pub async fn create(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    email: &str,
    password_hash: &str,
) -> Result<User, RepoError> {
    create_with_tos(pool, email, password_hash, None).await
}

/// Insert a user recording the accepted ToS version (P1-001).
/// `tos_version` is the `accept_tos_version` the client sent —
/// the caller has already checked it equals the current version;
/// this function records, it does not validate.
pub async fn create_with_tos(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    email: &str,
    password_hash: &str,
    tos_version: Option<&str>,
) -> Result<User, RepoError> {
    sqlx::query_as!(
        User,
        "INSERT INTO users (email, password_hash, tos_version) VALUES ($1, $2, $3)
         RETURNING id, email, password_hash, email_verified_at, role, status,
             tos_version, deleted_at, display_name, pending_email, settings, created_at, updated_at",
        email,
        password_hash,
        tos_version
    )
    .fetch_one(pool)
    .await
    .map_err(RepoError::from_sqlx)
}

/// Fetch by id. `None` when the id does not exist.
pub async fn by_id(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    id: Uuid,
) -> Result<Option<User>, RepoError> {
    sqlx::query_as!(
        User,
        "SELECT id, email, password_hash, email_verified_at, role, status,
             tos_version, deleted_at, display_name, pending_email, settings, created_at, updated_at
         FROM users WHERE id = $1",
        id
    )
    .fetch_optional(pool)
    .await
    .map_err(RepoError::from_sqlx)
}

/// Fetch by email. The column is citext, so the match is/// case-insensitive (`Alice@Example.com` finds `alice@example.com`).
///
/// The `$1::citext` cast is load-bearing: a bound `text` parameter
/// makes Postgres resolve `email = $1` as case-sensitive `text =
/// text` (the citext value is cast to text), so a differently-cased
/// lookup would silently miss. Casting the parameter forces the
/// citext comparison. (Caught by the P0-041 live test.)
pub async fn by_email(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    email: &str,
) -> Result<Option<User>, RepoError> {
    sqlx::query_as!(
        User,
        "SELECT id, email, password_hash, email_verified_at, role, status,
             tos_version, deleted_at, display_name, pending_email, settings, created_at, updated_at
         FROM users WHERE email = $1::citext",
        email
    )
    .fetch_optional(pool)
    .await
    .map_err(RepoError::from_sqlx)
}

/// Update the profile fields (P1-005): sets both columns to
/// the given values (`None` clears). The caller loads the user
/// first, applies the patch in Rust, and stores the final
/// values — there is no "leave unchanged" at this layer, so a
/// plain SET is correct (a COALESCE here could never clear a
/// field). The email is bound as `citext` (same load-bearing
/// cast as `by_email`). Returns `NotFound` when the user does
/// not exist. The caller validates (display-name shape, email
/// shape + disposable + duplicate) — this function stores,
/// it does not validate.
pub async fn update_profile(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    id: Uuid,
    display_name: Option<String>,
    pending_email: Option<String>,
) -> Result<User, RepoError> {
    sqlx::query_as!(
        User,
        "UPDATE users SET
             display_name = $2,
             pending_email = $3::citext,
             updated_at = now()
         WHERE id = $1
         RETURNING id, email, password_hash, email_verified_at, role, status,
             tos_version, deleted_at, display_name, pending_email, settings, created_at, updated_at",
        id,
        display_name,
        pending_email,
    )
    .fetch_one(pool)
    .await
    .map_err(|err| match err {
        sqlx::Error::RowNotFound => RepoError::NotFound,
        other => RepoError::from_sqlx(other),
    })
}
pub async fn set_email_verified(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    id: Uuid,
) -> Result<User, RepoError> {
    sqlx::query_as!(
        User,
        "UPDATE users SET email_verified_at = now() WHERE id = $1
         RETURNING id, email, password_hash, email_verified_at, role, status,
             tos_version, deleted_at, display_name, pending_email, settings, created_at, updated_at",
        id
    )
    .fetch_one(pool)
    .await
    .map_err(|err| match err {
        sqlx::Error::RowNotFound => RepoError::NotFound,
        other => RepoError::from_sqlx(other),
    })
}
