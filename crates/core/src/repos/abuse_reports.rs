//! Abuse-report repository (P1-083, FR-P-07).
//!
//! `abuse_reports` is the admin abuse queue's table (migration
//! 004): one row per report, `status` open | reviewing |
//! resolved. Two writers exist: the public intake (`POST
//! /v1/abuse`, reporter-filed, `user_id` NULL — P1-089, the
//! intake service files through [`insert_intake`]) and the
//! velocity detector (`detag_core::abuse`, system-raised,
//! `user_id` always set — P1-083 migration). This module is
//! both writers' side: insert a spike report, insert an
//! intake report, and find a user's still-open spike report
//! inside a window (the dedup scan — one open report per user
//! per window, so a sustained spike files once, not once per
//! job).

use super::error::RepoError;
use chrono::{DateTime, Utc};
use uuid::Uuid;

/// One abuse report row (the fields the detector and its tests
/// need). `reporter_contact` is the public intake's field; the
/// detector leaves it `NULL`.
#[derive(Debug, Clone, PartialEq)]
pub struct AbuseReport {
    pub id: Uuid,
    pub user_id: Option<Uuid>,
    pub reporter_contact: Option<String>,
    pub job_id: Option<Uuid>,
    pub reason: String,
    pub status: String,
    pub resolution: Option<String>,
    pub notes: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Insert a reporter-filed intake report (P1-089, FR-C-06):
/// `user_id` NULL (the reporter has no account — the intake
/// is public), `job_id` the reported job when the reporter
/// gave one (`None` = a general-abuse report, no job), the
/// reporter's contact address as filed, `status` `open`.
/// The caller checks the job exists first (404 when it does
/// not — ASSUMPTION-050); the column's foreign key is the
/// backstop. Returns the row (id + timestamps) for the 201 body.
pub async fn insert_intake(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    job_id: Option<Uuid>,
    reporter_contact: &str,
    reason: &str,
) -> Result<AbuseReport, RepoError> {
    sqlx::query_as!(
        AbuseReport,
        r#"INSERT INTO abuse_reports (job_id, reporter_contact, reason, status)
           VALUES ($1, $2, $3, 'open')
           RETURNING id, user_id, reporter_contact, job_id, reason, status,
                     resolution, notes, created_at, updated_at"#,
        job_id,
        reporter_contact,
        reason,
    )
    .fetch_one(pool)
    .await
    .map_err(RepoError::from_sqlx)
}

/// Insert a system-raised spike report: `user_id` set, `job_id`
/// NULL (the spike is about the user's velocity, not one job),
/// `status` `open`. Returns the row's id.
pub async fn insert_spike(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    user_id: Uuid,
    reason: &str,
    notes: &str,
) -> Result<Uuid, RepoError> {
    let row = sqlx::query!(
        r#"INSERT INTO abuse_reports (user_id, reason, notes, status)
           VALUES ($1, $2, $3, 'open')
           RETURNING id"#,
        user_id,
        reason,
        notes,
    )
    .fetch_one(pool)
    .await
    .map_err(RepoError::from_sqlx)?;
    Ok(row.id)
}

/// The user's most recent still-open report created at or after
/// `since` (`None` when there is none). The detector calls this
/// with the window start before filing, so a sustained spike
/// files once per window. `status = 'open'` only: a report an
/// admin already moved to reviewing/resolved is a handled
/// report, and a fresh spike deserves a fresh row.
pub async fn open_since(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    user_id: Uuid,
    since: DateTime<Utc>,
) -> Result<Option<AbuseReport>, RepoError> {
    sqlx::query_as!(
        AbuseReport,
        r#"SELECT id, user_id, reporter_contact, job_id, reason, status,
                  resolution, notes, created_at, updated_at
           FROM abuse_reports
           WHERE user_id = $1 AND status = 'open' AND created_at >= $2
           ORDER BY created_at DESC, id DESC
           LIMIT 1"#,
        user_id,
        since,
    )
    .fetch_optional(pool)
    .await
    .map_err(RepoError::from_sqlx)
}

/// Read one report back by id (tests + the admin queue's
/// follow-ups). `None` when the id does not exist.
pub async fn by_id(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    id: Uuid,
) -> Result<Option<AbuseReport>, RepoError> {
    sqlx::query_as!(
        AbuseReport,
        r#"SELECT id, user_id, reporter_contact, job_id, reason, status,
                  resolution, notes, created_at, updated_at
           FROM abuse_reports
           WHERE id = $1"#,
        id,
    )
    .fetch_optional(pool)
    .await
    .map_err(RepoError::from_sqlx)
}
