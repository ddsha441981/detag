//! Users repository (P0-041, E0.4). SQL lives here, not in services
//! (coding standards). `query_as!` macros are compile-time checked
//! against the live schema; offline data is checked in under
//! `.sqlx/` (see `just sqlx-prepare`).

use super::error::RepoError;
use crate::db::DbPool;
use chrono::{DateTime, Utc};
use uuid::Uuid;

/// A row of `users` (migration 001). `settings` is the raw jsonb
/// value; services interpret its keys.
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
    pub settings: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Insert a user. Duplicate email (case-insensitive — the column is
/// citext) fails with `RepoError::UniqueViolation` naming
/// `users_email_key`; a bad role/status fails with `CheckViolation`.
pub async fn create(pool: &DbPool, email: &str, password_hash: &str) -> Result<User, RepoError> {
    sqlx::query_as!(
        User,
        "INSERT INTO users (email, password_hash) VALUES ($1, $2)
         RETURNING id, email, password_hash, email_verified_at, role, status,
             tos_version, deleted_at, settings, created_at, updated_at",
        email,
        password_hash
    )
    .fetch_one(pool)
    .await
    .map_err(RepoError::from_sqlx)
}

/// Fetch by id. `None` when the id does not exist.
pub async fn by_id(pool: &DbPool, id: Uuid) -> Result<Option<User>, RepoError> {
    sqlx::query_as!(
        User,
        "SELECT id, email, password_hash, email_verified_at, role, status,
             tos_version, deleted_at, settings, created_at, updated_at
         FROM users WHERE id = $1",
        id
    )
    .fetch_optional(pool)
    .await
    .map_err(RepoError::from_sqlx)
}

/// Fetch by email. The column is citext, so the match is
/// case-insensitive (`Alice@Example.com` finds `alice@example.com`).
///
/// The `$1::citext` cast is load-bearing: a bound `text` parameter
/// makes Postgres resolve `email = $1` as case-sensitive `text =
/// text` (the citext value is cast to text), so a differently-cased
/// lookup would silently miss. Casting the parameter forces the
/// citext comparison. (Caught by the P0-041 live test.)
pub async fn by_email(pool: &DbPool, email: &str) -> Result<Option<User>, RepoError> {
    sqlx::query_as!(
        User,
        "SELECT id, email, password_hash, email_verified_at, role, status,
             tos_version, deleted_at, settings, created_at, updated_at
         FROM users WHERE email = $1::citext",
        email
    )
    .fetch_optional(pool)
    .await
    .map_err(RepoError::from_sqlx)
}
