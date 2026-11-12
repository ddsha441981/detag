//! Artifacts key lookups (P0-060, E0.6). SQL lives here, not in
//! services (coding standards). `query_scalar!` macros are
//! compile-time checked against the live schema, with offline data
//! checked in under `.sqlx/` (run `just sqlx-prepare` after
//! changing a query; CI runs `just sqlx-prepare-check`).
//!
//! This repo is deliberately narrow: the GC job (detag-gc) is the
//! only caller, and it only needs storage keys — never full rows.
//! [`expired_storage_keys`] is the per-plan retention source:
//! every artifact row carries `expires_at` (now + plan TTL at
//! creation, FR-J-09), so the GC deletes exactly the keys whose
//! expiry passed, without knowing any plan. [`all_storage_keys`]
//! is the orphan detector's "known" set: a key the database
//! references — as an artifact OR as an upload's input — is not
//! an orphan. Inputs have no `expires_at`, so the GC never deletes
//! them; the ILM 3-day ceiling (ops/minio/lifecycle.json) is the
//! input backstop.

use super::error::RepoError;
use chrono::{DateTime, Utc};
use sqlx::{Executor, Postgres};

/// Storage keys of artifacts whose retention expired at `now`
/// (`expires_at <= now`). One row per key.
pub async fn expired_storage_keys<'e, E>(
    executor: E,
    now: DateTime<Utc>,
) -> Result<Vec<String>, RepoError>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query_scalar!(
        "SELECT storage_key FROM artifacts WHERE expires_at <= $1",
        now
    )
    .fetch_all(executor)
    .await
    .map_err(RepoError::from_sqlx)
}

/// Every storage key the database knows about: artifact keys plus
/// upload input keys. The GC treats any of these as "known" (not
/// an orphan); anything else in the bucket is an orphan.
pub async fn all_storage_keys<'e, E>(executor: E) -> Result<Vec<String>, RepoError>
where
    E: Executor<'e, Database = Postgres>,
{
    // The `!` on the output column tells sqlx the UNION result is
    // NOT NULL: both sources are NOT NULL columns, but a UNION's
    // output is nullable in sqlx's inference without it.
    sqlx::query_scalar!(
        "SELECT storage_key AS \"storage_key!\" FROM artifacts \
         UNION SELECT storage_key FROM uploads"
    )
    .fetch_all(executor)
    .await
    .map_err(RepoError::from_sqlx)
}
