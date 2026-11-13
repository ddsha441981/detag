//! Jobs repository (P0-041, E0.4). SQL lives here, not in services
//! (coding standards). `query_as!` macros are compile-time checked
//! against the live schema; offline data is checked in under
//! `.sqlx/` (see `just sqlx-prepare`).
//!
//! The repo is deliberately dumb about the job state machine: it
//! inserts, fetches and updates the status column. Which transitions
//! are legal is the state machine's job (`state_machine.rs`,
//! Phase 3 pipeline). One status value is defined here already:
//! `held` (P1-008) — a queued job of a suspended account, held
//! until restore releases it back to `queued`. The Phase 2
//! intake/claim path must treat `held` as "do not start".

use super::error::RepoError;
use chrono::{DateTime, Utc};
use uuid::Uuid;

/// A row of `jobs` (migration 002). `params` and `error` are the raw
/// jsonb values; `progress` is 0.0-1.0.
#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    pub id: Uuid,
    pub user_id: Uuid,
    pub upload_id: Option<Uuid>,
    pub kind: String,
    pub mode: String,
    pub params: serde_json::Value,
    pub status: String,
    pub stage: Option<String>,
    pub progress: f32,
    pub priority: i32,
    pub lane: String,
    pub attempts: i32,
    pub idempotency_key: Option<String>,
    pub error: Option<serde_json::Value>,
    pub attestation_id: Option<Uuid>,
    pub webhook_url: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Fields for `create`. Everything the INSERT needs; the rest
/// (status timestamps, attempts, progress) starts at its default.
#[derive(Debug, Clone)]
pub struct NewJob<'a> {
    pub user_id: Uuid,
    pub upload_id: Option<Uuid>,
    pub kind: &'a str,
    pub mode: &'a str,
    pub status: &'a str,
    pub lane: &'a str,
    pub params: serde_json::Value,
    pub idempotency_key: Option<&'a str>,
}

/// Insert a job. A duplicate `idempotency_key` fails with
/// `RepoError::UniqueViolation`; a bad kind with `CheckViolation`;
/// an unknown user/upload with `ForeignKeyViolation`.
pub async fn create(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    job: &NewJob<'_>,
) -> Result<Job, RepoError> {
    sqlx::query_as!(
        Job,
        "INSERT INTO jobs (user_id, upload_id, kind, mode, status, lane, params, idempotency_key)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
         RETURNING id, user_id, upload_id, kind, mode, params, status, stage,
             progress, priority, lane, attempts, idempotency_key, error, attestation_id,
             webhook_url, started_at, finished_at, created_at, updated_at",
        job.user_id,
        job.upload_id,
        job.kind,
        job.mode,
        job.status,
        job.lane,
        job.params,
        job.idempotency_key
    )
    .fetch_one(pool)
    .await
    .map_err(RepoError::from_sqlx)
}

/// Fetch by id. `None` when the id does not exist.
pub async fn by_id(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    id: Uuid,
) -> Result<Option<Job>, RepoError> {
    sqlx::query_as!(
        Job,
        "SELECT id, user_id, upload_id, kind, mode, params, status, stage,
             progress, priority, lane, attempts, idempotency_key, error, attestation_id,
             webhook_url, started_at, finished_at, created_at, updated_at
         FROM jobs WHERE id = $1",
        id
    )
    .fetch_optional(pool)
    .await
    .map_err(RepoError::from_sqlx)
}

/// Newest-first page of a user's jobs. `limit` caps the page.
pub async fn list_by_user(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    user_id: Uuid,
    limit: i64,
) -> Result<Vec<Job>, RepoError> {
    sqlx::query_as!(
        Job,
        "SELECT id, user_id, upload_id, kind, mode, params, status, stage,
             progress, priority, lane, attempts, idempotency_key, error, attestation_id,
             webhook_url, started_at, finished_at, created_at, updated_at
         FROM jobs WHERE user_id = $1 ORDER BY created_at DESC LIMIT $2",
        user_id,
        limit
    )
    .fetch_all(pool)
    .await
    .map_err(RepoError::from_sqlx)
}

/// Set the status (and bump `updated_at`). `NotFound` when the id
/// does not exist. No transition validation here — see the module
/// docs.
pub async fn set_status(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    id: Uuid,
    status: &str,
) -> Result<Job, RepoError> {
    sqlx::query_as!(
        Job,
        "UPDATE jobs SET status = $2, updated_at = now() WHERE id = $1
         RETURNING id, user_id, upload_id, kind, mode, params, status, stage,
             progress, priority, lane, attempts, idempotency_key, error, attestation_id,
             webhook_url, started_at, finished_at, created_at, updated_at",
        id,
        status
    )
    .fetch_optional(pool)
    .await
    .map_err(RepoError::from_sqlx)?
    .ok_or(RepoError::NotFound)
}

/// Hold a suspended user's queued jobs (P1-008): every job
/// with status `queued` becomes `held`, and returns how many
/// were held. Only `queued` jobs move — a job already running
/// (or terminal) is left alone; the worker finishes it. A
/// `held` job is not runnable: the Phase 2 intake/claim path
/// must treat `held` as "do not start" (there is no claim
/// query to teach yet — the worker is Redis/apalis-driven and
/// the Postgres status is a record until Phase 2 wires the
/// claim). Restore releases them (`release_held_for_user`).
pub async fn hold_queued_for_user(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    user_id: Uuid,
) -> Result<u64, RepoError> {
    let result = sqlx::query!(
        "UPDATE jobs SET status = 'held', updated_at = now()
         WHERE user_id = $1 AND status = 'queued'",
        user_id,
    )
    .execute(pool)
    .await
    .map_err(RepoError::from_sqlx)?;
    Ok(result.rows_affected())
}

/// Release a restored user's held jobs (P1-008): every job
/// with status `held` goes back to `queued`, and returns how
/// many were released. The jobs re-enter the queue in their
/// original order (their `created_at` never changed — the
/// hold is a status flip, not a re-queue).
pub async fn release_held_for_user(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    user_id: Uuid,
) -> Result<u64, RepoError> {
    let result = sqlx::query!(
        "UPDATE jobs SET status = 'queued', updated_at = now()
         WHERE user_id = $1 AND status = 'held'",
        user_id,
    )
    .execute(pool)
    .await
    .map_err(RepoError::from_sqlx)?;
    Ok(result.rows_affected())
}
