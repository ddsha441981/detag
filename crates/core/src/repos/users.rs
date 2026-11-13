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

/// Swap the email to the pending address (P1-006): the address
/// moves from `pending_email` to `email`, the pending intent is
/// cleared, and `email_verified_at` is set — the token that
/// authorized this swap was delivered to the new address, so
/// the new address is verified by it. The `pending_email IS
/// NOT NULL` guard is the race guard: if the intent was cleared
/// (or already swapped) concurrently, zero rows update and the
/// caller gets `NotFound` instead of swapping a stale or empty
/// address. A `UniqueViolation` means the pending address was
/// taken by another account while it was pending — the swap is
/// the enforcement point for the P1-005 intent-time duplicate
/// check (which was only a courtesy).
pub async fn swap_email(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    id: Uuid,
) -> Result<User, RepoError> {
    sqlx::query_as!(
        User,
        "UPDATE users SET
             email = pending_email,
             pending_email = NULL,
             email_verified_at = now(),
             updated_at = now()
         WHERE id = $1 AND pending_email IS NOT NULL
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

/// The deletion grace (P1-007): 30 days from the deletion
/// request until the nightly purge hard-deletes the account.
/// A product-policy constant, not config — no doc specified a
/// different value, and the purge, the api-spec, and the
/// data-model doc all cite this one number.
pub const DELETION_GRACE_DAYS: i64 = 30;

/// Request account deletion (P1-007): the account enters the
/// grace (`status = 'deletion_pending'`, `deleted_at = now()`).
/// The caller owns idempotency: a second request for an
/// already-pending account is a no-op at the service layer
/// (this function would refresh `deleted_at` — the grace runs
/// from the FIRST request, never restarted by re-requesting).
pub async fn request_deletion(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    id: Uuid,
) -> Result<User, RepoError> {
    sqlx::query_as!(
        User,
        "UPDATE users SET
             status = 'deletion_pending',
             deleted_at = now(),
             updated_at = now()
         WHERE id = $1
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

/// Cancel a pending deletion (P1-007 — the login hook): the
/// account is active again, `deleted_at` cleared. Returns
/// `None` when the account is not pending (already active, or
/// a concurrent login cancelled it first) — that is not an
/// error, it is the goal state already holding.
pub async fn cancel_deletion(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    id: Uuid,
) -> Result<Option<User>, RepoError> {
    sqlx::query_as!(
        User,
        "UPDATE users SET
             status = 'active',
             deleted_at = NULL,
             updated_at = now()
         WHERE id = $1 AND status = 'deletion_pending'
         RETURNING id, email, password_hash, email_verified_at, role, status,
             tos_version, deleted_at, display_name, pending_email, settings, created_at, updated_at",
        id
    )
    .fetch_optional(pool)
    .await
    .map_err(RepoError::from_sqlx)
}

/// The users the nightly purge may hard-delete (P1-007):
/// `deletion_pending` with `deleted_at` before `cutoff`
/// (usually now − 30 days). Oldest first, so a bounded purge
/// run takes the longest-waiting accounts first.
pub async fn purgeable(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    cutoff: DateTime<Utc>,
) -> Result<Vec<Uuid>, RepoError> {
    sqlx::query_scalar!(
        "SELECT id FROM users
         WHERE status = 'deletion_pending' AND deleted_at < $1
         ORDER BY deleted_at ASC",
        cutoff,
    )
    .fetch_all(pool)
    .await
    .map_err(RepoError::from_sqlx)
}

/// Hard-delete one user (P1-007, the purge's last step). The
/// database cascades do the work: sessions, jobs (+ artifacts,
/// job events, quality reports via jobs), uploads,
/// subscriptions, usage counters, idempotency keys, and
/// verification tokens are `ON DELETE CASCADE`; audit rows
/// survive with `actor_user_id` intact (no FK on the actor —
/// migration 20261113062352 dropped it: the actor is a
/// historical fact, and the FK's ON DELETE SET NULL was an
/// UPDATE the append-only trigger forbids, so the purge could
/// never delete a user with audit rows); outbox rows survive
/// (they are addressed by hash, not by user). Returns `false`
/// when the user was already gone (a concurrent purge took it
/// first).
pub async fn delete_user(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    id: Uuid,
) -> Result<bool, RepoError> {
    let result = sqlx::query!("DELETE FROM users WHERE id = $1", id)
        .execute(pool)
        .await
        .map_err(RepoError::from_sqlx)?;
    Ok(result.rows_affected() > 0)
}
