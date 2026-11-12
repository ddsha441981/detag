//! Uploads repository (P0-057, E0.6). SQL lives here, not in services
//! (coding standards). `query_as!` macros are compile-time checked
//! against the live schema, with offline data checked in under
//! `.sqlx/` (run `just sqlx-prepare` after changing a query; CI runs
//! `just sqlx-prepare-check`).
//!
//! The repo is deliberately dumb about the upload state machine: it
//! inserts, fetches and applies guarded status updates. Which
//! transitions are legal — and the part count/size verification on
//! complete — is the state machine's job (`state_machine.rs`).
//! [`set_status_guarded`] is the only way to move an upload's
//! status: it updates only when the row is still in the `from`
//! status and returns `None` when the guard did not match, so a
//! concurrent transition cannot slip through. Callers that need to
//! tell "missing row" from "wrong state" fetch first with
//! [`by_id`] (the Phase 3 upload service does exactly that, then
//! maps the outcome to `AppError::NotFound` /
//! `AppError::InvalidStateTransition`).

use super::error::RepoError;
use chrono::{DateTime, Utc};
use uuid::Uuid;

/// A row of `uploads` (migration 002). `sha256` is the client's
/// reported hash at creation (verified on complete in P0-058);
/// `storage_key` is the ADR-0011 key; `parts` is the expected
/// part count.
#[derive(Debug, Clone, PartialEq)]
pub struct Upload {
    pub id: Uuid,
    pub user_id: Uuid,
    pub kind: String,
    pub filename: String,
    pub bytes: i64,
    pub sha256: String,
    pub phash: Option<String>,
    pub mime: String,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub duration_s: Option<f32>,
    pub metadata: serde_json::Value,
    pub status: String,
    pub storage_key: String,
    pub parts: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Fields for `create`. The status always starts at
/// `pending_parts` (hardcoded in the INSERT — an upload has
/// exactly one birth state).
#[derive(Debug, Clone)]
pub struct NewUpload<'a> {
    pub user_id: Uuid,
    pub kind: &'a str,
    pub filename: &'a str,
    pub bytes: i64,
    pub sha256: &'a str,
    pub mime: &'a str,
    pub storage_key: &'a str,
    pub parts: i32,
}

/// Insert an upload in `pending_parts`. A bad kind fails with
/// `RepoError::CheckViolation`; an unknown user with
/// `ForeignKeyViolation`.
pub async fn create(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    upload: &NewUpload<'_>,
) -> Result<Upload, RepoError> {
    sqlx::query_as!(
        Upload,
        "INSERT INTO uploads (user_id, kind, filename, bytes, sha256, mime, storage_key, parts)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
         RETURNING id, user_id, kind, filename, bytes, sha256, phash, mime, width, height,
             duration_s, metadata, status, storage_key, parts, created_at, updated_at",
        upload.user_id,
        upload.kind,
        upload.filename,
        upload.bytes,
        upload.sha256,
        upload.mime,
        upload.storage_key,
        upload.parts
    )
    .fetch_one(pool)
    .await
    .map_err(RepoError::from_sqlx)
}

/// Fetch one upload by id (`None` when it does not exist).
pub async fn by_id(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    id: Uuid,
) -> Result<Option<Upload>, RepoError> {
    sqlx::query_as!(
        Upload,
        "SELECT id, user_id, kind, filename, bytes, sha256, phash, mime, width, height,
             duration_s, metadata, status, storage_key, parts, created_at, updated_at
         FROM uploads WHERE id = $1",
        id
    )
    .fetch_optional(pool)
    .await
    .map_err(RepoError::from_sqlx)
}

/// Move an upload from `from` to `to` (and bump `updated_at`),
/// atomically: the UPDATE matches only a row still in `from`.
/// Returns the updated row, or `None` when no row matched — the
/// id does not exist, or the upload is already in another state.
/// Callers fetch with [`by_id`] first when they need to tell
/// those apart.
pub async fn set_status_guarded(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    id: Uuid,
    from: &str,
    to: &str,
) -> Result<Option<Upload>, RepoError> {
    sqlx::query_as!(
        Upload,
        "UPDATE uploads SET status = $3, updated_at = now()
         WHERE id = $1 AND status = $2
         RETURNING id, user_id, kind, filename, bytes, sha256, phash, mime, width, height,
             duration_s, metadata, status, storage_key, parts, created_at, updated_at",
        id,
        from,
        to
    )
    .fetch_optional(pool)
    .await
    .map_err(RepoError::from_sqlx)
}
