//! API keys repository (P1-033, FR-A-06). `api_keys` is the
//! machine-credential table (migration 004, data-model doc):
//! one row per key, keyed by the sha256 of the presented
//! secret (`secret_hash`) — the raw secret is never stored.
//! A key is live while `revoked_at` is NULL; revocation
//! (P1-036) sets it, rotation (P1-035) creates a new row and
//! revokes the old one after the overlap window. `prefix` is
//! the display identifier (`pf_live_` + first 4 secret chars,
//! see `detag_core::api_keys`); listings show prefix + last4
//! only, never the secret.
//!
//! The per-user cap (10 live keys, P1-033) is enforced in the
//! service, not here: [`count_live_for_user`] counts the live
//! rows, and the service holds the user's row lock
//! ([`lock_user_for_key_change`]) across the count + insert so
//! two concurrent creates cannot both pass the cap.

use super::error::RepoError;
use chrono::{DateTime, Utc};
use uuid::Uuid;

/// A stored API key row.
#[derive(Debug, Clone, PartialEq)]
pub struct ApiKey {
    pub id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    /// Display identifier: `pf_live_` + first 4 secret chars.
    pub prefix: String,
    /// sha256 hex of the FULL presented secret (prefix included).
    pub secret_hash: String,
    pub scopes: Vec<String>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub rate_limit_tier: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// What [`create`] inserts. The caller generates the secret
/// (see `detag_core::api_keys`) and passes its hash + display
/// prefix; this module never sees the raw secret.
pub struct NewApiKey {
    pub user_id: Uuid,
    pub name: String,
    pub prefix: String,
    pub secret_hash: String,
    pub scopes: Vec<String>,
}

/// Insert a key row. The secret hash must be unique — a
/// collision is a `RepoError::UniqueViolation` (in practice
/// impossible: the secret is 24 random bytes).
pub async fn create(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    new: &NewApiKey,
) -> Result<ApiKey, RepoError> {
    sqlx::query_as!(
        ApiKey,
        r#"INSERT INTO api_keys (user_id, name, prefix, secret_hash, scopes)
           VALUES ($1, $2, $3, $4, $5)
           RETURNING id, user_id, name, prefix, secret_hash, scopes,
               last_used_at, revoked_at, rate_limit_tier, created_at, updated_at"#,
        new.user_id,
        new.name,
        new.prefix,
        new.secret_hash,
        &new.scopes,
    )
    .fetch_one(pool)
    .await
    .map_err(RepoError::from_sqlx)
}

/// Look a key up by the sha256 of the presented secret
/// (P1-034, FR-A-06): the extractor hashes exactly what the
/// client presented and reads this row. LIVE keys only
/// (`revoked_at IS NULL`) — a revoked key must not verify:
/// revocation (P1-036) is the kill path, and a revoked hash
/// answers exactly like an unknown one (401 `PF-1001`), so
/// the response never says whether a key existed, was
/// revoked, or never minted. The unique index on
/// `secret_hash` (migration `20261114030933`) makes the
/// lookup unambiguous.
pub async fn by_secret_hash(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    secret_hash: &str,
) -> Result<Option<ApiKey>, RepoError> {
    sqlx::query_as!(
        ApiKey,
        r#"SELECT id, user_id, name, prefix, secret_hash, scopes,
               last_used_at, revoked_at, rate_limit_tier, created_at, updated_at
           FROM api_keys
           WHERE secret_hash = $1 AND revoked_at IS NULL"#,
        secret_hash,
    )
    .fetch_optional(pool)
    .await
    .map_err(RepoError::from_sqlx)
}

/// Record a use of a live key (P1-034): set `last_used_at`
/// to `now`. The caller throttles this — the extractor calls
/// it only when the row's value is stale (older than
/// [`crate::api_keys::API_KEY_LAST_USED_THROTTLE_SECS`]) —
/// so a hot key does not pay a write per request. Unconditional
/// here: the throttle decision belongs to the caller, which
/// has the row; this function just writes.
pub async fn touch_last_used_at(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    key_id: Uuid,
    now: DateTime<Utc>,
) -> Result<(), RepoError> {
    sqlx::query!(
        r#"UPDATE api_keys SET last_used_at = $1, updated_at = $1 WHERE id = $2"#,
        now,
        key_id,
    )
    .execute(pool)
    .await
    .map_err(RepoError::from_sqlx)?;
    Ok(())
}

/// Count a user's LIVE keys (`revoked_at IS NULL`) — the
/// P1-033 cap input. Revoked keys do not spend the budget:
/// a user who revoked 9 of 10 keys can mint 9 more.
pub async fn count_live_for_user(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    user_id: Uuid,
) -> Result<i64, RepoError> {
    let row = sqlx::query!(
        r#"SELECT COUNT(*) AS "count!"
           FROM api_keys
           WHERE user_id = $1 AND revoked_at IS NULL"#,
        user_id,
    )
    .fetch_one(pool)
    .await
    .map_err(RepoError::from_sqlx)?;
    Ok(row.count)
}

/// Lock a user's row for a key change (create / rotate /
/// revoke): `SELECT ... FOR UPDATE` serialises concurrent
/// creates for the same user, so the cap check + insert in
/// the service's transaction cannot both pass the cap and
/// overshoot it. The lock is on the `users` row — the
/// `api_keys` rows themselves cannot be locked before they
/// exist. A missing user is `RepoError::NotFound`.
pub async fn lock_user_for_key_change(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    user_id: Uuid,
) -> Result<(), RepoError> {
    let row = sqlx::query!(
        r#"SELECT id AS "id!" FROM users WHERE id = $1 FOR UPDATE"#,
        user_id,
    )
    .fetch_optional(pool)
    .await
    .map_err(RepoError::from_sqlx)?;
    match row {
        Some(_) => Ok(()),
        None => Err(RepoError::NotFound),
    }
}
