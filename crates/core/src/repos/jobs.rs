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
use crate::pagination::Cursor;
use chrono::{DateTime, NaiveDate, Utc};
use uuid::Uuid;

/// A row of `jobs` (migration 002). `params` and `error` are the raw
/// jsonb values; `progress` is 0.0-1.0. `key_id` (P1-038) is the
/// API key that created the job — `None` for a job created via
/// the web console / JWT (no key involved).
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
    /// The API key that created the job (`None` = web / JWT).
    pub key_id: Option<Uuid>,
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
    /// The API key that created the job (`None` = web / JWT).
    pub key_id: Option<Uuid>,
}

/// One key's usage for one UTC day (P1-038): how many of the
/// key's jobs were created that day, by kind. The usage
/// endpoints (P1-080) consume this — it is the per-key
/// breakdown's source row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyUsage {
    /// Image jobs created that day by the key.
    pub images: i64,
    /// Video jobs created that day by the key.
    pub video_jobs: i64,
}

/// One row of a user's per-key usage breakdown for one UTC day
/// (P1-038): the same counts as [`KeyUsage`], plus which key
/// they belong to. `key_id` is `None` for the unattributed row
/// (jobs created via the web console / JWT).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyUsageRow {
    /// The key, or `None` for unattributed jobs.
    pub key_id: Option<Uuid>,
    /// The key's display prefix (`pf_live_…`), or `None` for
    /// the unattributed row.
    pub key_prefix: Option<String>,
    /// Image jobs created that day by the key.
    pub images: i64,
    /// Video jobs created that day by the key.
    pub video_jobs: i64,
}

/// One row of the admin jobs list (P1-038): the job's columns
/// (flat, like [`UserSearchRow`] — `query_as!` cannot map
/// into a nested struct) plus the display prefix of the key
/// that created it (`None` for a web / JWT-created job — the
/// prefix is the only key material the admin view ever shows;
/// never the secret or its hash).
#[derive(Debug, Clone, PartialEq)]
pub struct AdminJobRow {
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
    /// The creating key's id (`None` = web / JWT).
    pub key_id: Option<Uuid>,
    /// The creating key's display prefix, or `None`.
    pub key_prefix: Option<String>,
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
        "INSERT INTO jobs (user_id, upload_id, kind, mode, status, lane, params, idempotency_key, key_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
         RETURNING id, user_id, upload_id, kind, mode, params, status, stage,
             progress, priority, lane, attempts, idempotency_key, error, attestation_id,
             webhook_url, started_at, finished_at, created_at, updated_at, key_id",
        job.user_id,
        job.upload_id,
        job.kind,
        job.mode,
        job.status,
        job.lane,
        job.params,
        job.idempotency_key,
        job.key_id
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
             webhook_url, started_at, finished_at, created_at, updated_at, key_id
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
             webhook_url, started_at, finished_at, created_at, updated_at, key_id
         FROM jobs WHERE user_id = $1 ORDER BY created_at DESC LIMIT $2",
        user_id,
        limit
    )
    .fetch_all(pool)
    .await
    .map_err(RepoError::from_sqlx)
}

/// Newest-first page of one key's jobs (P1-038). `limit` caps
/// the page. This is the attribution read: a key's jobs, and
/// only that key's — the usage endpoints filter by key through
/// here (and through [`usage_by_key`] for the counts).
pub async fn list_by_key(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    key_id: Uuid,
    limit: i64,
) -> Result<Vec<Job>, RepoError> {
    sqlx::query_as!(
        Job,
        "SELECT id, user_id, upload_id, kind, mode, params, status, stage,
             progress, priority, lane, attempts, idempotency_key, error, attestation_id,
             webhook_url, started_at, finished_at, created_at, updated_at, key_id
         FROM jobs WHERE key_id = $1 ORDER BY created_at DESC LIMIT $2",
        key_id,
        limit
    )
    .fetch_all(pool)
    .await
    .map_err(RepoError::from_sqlx)
}

/// One key's usage for one UTC day (P1-038): the count of the
/// key's jobs created that day, by kind. The day window is
/// computed in Rust (`day` 00:00 UTC to the next 00:00 UTC),
/// never in SQL — a `created_at::date` comparison would read
/// the session timezone and attribute a job to the wrong day.
/// A key with no jobs that day reads as zeros, not `None`.
pub async fn usage_by_key(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    key_id: Uuid,
    day: NaiveDate,
) -> Result<KeyUsage, RepoError> {
    let start = day
        .and_hms_opt(0, 0, 0)
        .expect("midnight is a valid time")
        .and_utc();
    let end = start + chrono::Duration::days(1);
    let row = sqlx::query!(
        "SELECT COUNT(*) FILTER (WHERE kind = 'image') AS \"images!\",
                COUNT(*) FILTER (WHERE kind = 'video') AS \"video_jobs!\"
         FROM jobs WHERE key_id = $1 AND created_at >= $2 AND created_at < $3",
        key_id,
        start,
        end,
    )
    .fetch_one(pool)
    .await
    .map_err(RepoError::from_sqlx)?;
    Ok(KeyUsage {
        images: row.images,
        video_jobs: row.video_jobs,
    })
}

/// A user's per-key usage breakdown for one UTC day (P1-038):
/// one row per key that created a job that day, plus the
/// unattributed row (`key_id` NULL) when a web / JWT-created
/// job exists. The prefix rides along so the caller never
/// needs a second query to label the rows — this is the shape
/// P1-080's per-key breakdown returns. Empty when the user
/// created nothing that day (no rows, not a zero row).
pub async fn usage_breakdown_for_user(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    user_id: Uuid,
    day: NaiveDate,
) -> Result<Vec<KeyUsageRow>, RepoError> {
    let start = day
        .and_hms_opt(0, 0, 0)
        .expect("midnight is a valid time")
        .and_utc();
    let end = start + chrono::Duration::days(1);
    sqlx::query_as!(
        KeyUsageRow,
        "SELECT j.key_id, MAX(k.prefix) AS key_prefix,
                COUNT(*) FILTER (WHERE j.kind = 'image') AS \"images!\",
                COUNT(*) FILTER (WHERE j.kind = 'video') AS \"video_jobs!\"
         FROM jobs j LEFT JOIN api_keys k ON k.id = j.key_id
         WHERE j.user_id = $1 AND j.created_at >= $2 AND j.created_at < $3
         GROUP BY j.key_id
         ORDER BY \"images!\" DESC, \"video_jobs!\" DESC",
        user_id,
        start,
        end,
    )
    .fetch_all(pool)
    .await
    .map_err(RepoError::from_sqlx)
}

/// The admin jobs list (P1-038): newest-first page of jobs,
/// each with the display prefix of the key that created it
/// (`None` for web / JWT-created jobs). `user_id` / `key_id`
/// narrow the list (both `None` lists everything); `cursor`
/// is the keyset from the previous page (same
/// `(created_at, id)` shape as the users search, with
/// the same `date_trunc('milliseconds', …)` comparison — the
/// cursor stores Unix millis while `created_at` carries
/// microseconds). `limit` is the `limit + 1` fetch size: the
/// caller builds the page with `Page::from_window`.
pub async fn list_for_admin(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    user_id: Option<Uuid>,
    key_id: Option<Uuid>,
    cursor: Option<Cursor>,
    limit: i64,
) -> Result<Vec<AdminJobRow>, RepoError> {
    // One query per cursor case (the users search does the
    // same — a single query with an OR'd cursor predicate
    // defeats the index and reads worse).
    match cursor {
        Some(cursor) => {
            let cursor_ts = DateTime::from_timestamp_millis(cursor.ts)
                .expect("a cursor ts is a valid Unix-millis timestamp");
            sqlx::query_as!(
                AdminJobRow,
                r#"SELECT j.id, j.user_id, j.upload_id, j.kind, j.mode, j.params, j.status,
                     j.stage, j.progress, j.priority, j.lane, j.attempts, j.idempotency_key,
                     j.error, j.attestation_id, j.webhook_url, j.started_at, j.finished_at,
                     j.created_at, j.updated_at, j.key_id, k.prefix AS "key_prefix?"
                 FROM jobs j LEFT JOIN api_keys k ON k.id = j.key_id
                 WHERE ($1::uuid IS NULL OR j.user_id = $1)
                   AND ($2::uuid IS NULL OR j.key_id = $2)
                   AND (date_trunc('milliseconds', j.created_at), j.id) < ($3, $4)
                 ORDER BY date_trunc('milliseconds', j.created_at) DESC, j.id DESC
                 LIMIT $5"#,
                user_id,
                key_id,
                cursor_ts,
                cursor.id,
                limit,
            )
            .fetch_all(pool)
            .await
            .map_err(RepoError::from_sqlx)
        }
        None => sqlx::query_as!(
            AdminJobRow,
            r#"SELECT j.id, j.user_id, j.upload_id, j.kind, j.mode, j.params, j.status,
                 j.stage, j.progress, j.priority, j.lane, j.attempts, j.idempotency_key,
                 j.error, j.attestation_id, j.webhook_url, j.started_at, j.finished_at,
                 j.created_at, j.updated_at, j.key_id, k.prefix AS "key_prefix?"
             FROM jobs j LEFT JOIN api_keys k ON k.id = j.key_id
             WHERE ($1::uuid IS NULL OR j.user_id = $1)
               AND ($2::uuid IS NULL OR j.key_id = $2)
             ORDER BY j.created_at DESC, j.id DESC
             LIMIT $3"#,
            user_id,
            key_id,
            limit,
        )
        .fetch_all(pool)
        .await
        .map_err(RepoError::from_sqlx),
    }
}

/// How many of a user's jobs currently occupy a concurrency
/// slot (P1-051): `queued` or `running`. `held` jobs do NOT
/// count — `hold_queued_for_user` parks them, they hold no
/// worker — and neither do finished / failed / cancelled jobs
/// (their status is whatever the Phase 2/3 lifecycle wrote;
/// the slot is freed when the job leaves `queued`/`running`).
/// The count is a point-in-time read, not a reservation: the
/// crash-safe slot acquire/release with TTL is P1-053's, and
/// the atomic check-and-record lands with real job creation
/// in Phase 2 (ASSUMPTION-017).
pub async fn count_running_for_user(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    user_id: Uuid,
) -> Result<i64, RepoError> {
    let row = sqlx::query!(
        "SELECT COUNT(*) AS \"count!\" FROM jobs \
         WHERE user_id = $1 AND status IN ('queued', 'running')",
        user_id,
    )
    .fetch_one(pool)
    .await
    .map_err(RepoError::from_sqlx)?;
    Ok(row.count)
}

/// How many jobs a user created at or after `since` (P1-083):
/// the velocity detector's count. Every status counts — a
/// spike is about creation velocity, not about how far the
/// jobs got (a spammer's jobs fail fast, and failed jobs must
/// still trip the rule). The count is a point-in-time read:
/// a job created between this read and the report insert is
/// caught by the next check, not this one.
pub async fn count_created_since(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    user_id: Uuid,
    since: DateTime<Utc>,
) -> Result<i64, RepoError> {
    let row = sqlx::query!(
        "SELECT COUNT(*) AS \"count!\" FROM jobs \
         WHERE user_id = $1 AND created_at >= $2",
        user_id,
        since,
    )
    .fetch_one(pool)
    .await
    .map_err(RepoError::from_sqlx)?;
    Ok(row.count)
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
             webhook_url, started_at, finished_at, created_at, updated_at, key_id",
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
