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
//! job). The admin queue (P1-090) reads through
//! [`list_filtered`] and resolves through [`mark_reviewing`] /
//! [`resolve`] — the resolution stamps `resolved_at` /
//! `resolved_by` (migration `20261121001300`), so the SLA
//! check can tell a late resolution from an on-time one.

use super::error::RepoError;
use crate::pagination::Cursor;
use chrono::{DateTime, Utc};
use uuid::Uuid;

/// One abuse report row (the fields the detector and its tests
/// need). `reporter_contact` is the public intake's field; the
/// detector leaves it `NULL`. `resolved_at` / `resolved_by`
/// are set only by a resolve (P1-090) — NULL while the report
/// is open or reviewing.
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
    pub resolved_at: Option<DateTime<Utc>>,
    pub resolved_by: Option<Uuid>,
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
                     resolution, notes, created_at, updated_at, resolved_at,
                     resolved_by"#,
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
                  resolution, notes, created_at, updated_at, resolved_at,
                  resolved_by
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
                  resolution, notes, created_at, updated_at, resolved_at,
                  resolved_by
           FROM abuse_reports
           WHERE id = $1"#,
        id,
    )
    .fetch_optional(pool)
    .await
    .map_err(RepoError::from_sqlx)
}

/// List the queue newest first, keyset-paginated over
/// `(created_at, id)` (the P1-088 audit-list precedent — the
/// same `date_trunc('milliseconds', ...)` comparison, because
/// the cursor carries Unix milliseconds). `status` filters to
/// one queue state (`open` | `reviewing` | `resolved`) when
/// given — `None` lists the whole queue. `user_id` filters
/// to reports naming that user (P1-098: the attestation
/// review queue links here per user) — `None` lists every
/// report; an unknown user is an empty page, not a 404 (a
/// list never 404s on its filter — the P1-088 audit
/// precedent). The caller validates the status value (an
/// unknown one is a 422, never an empty page) and
/// range-checks the cursor's `ts` first (the repo's
/// `from_timestamp_millis(...).expect(...)` would panic on an
/// out-of-range `ts` — the P1-088 adoption-review lesson).
pub async fn list_filtered(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    status: Option<&str>,
    user_id: Option<Uuid>,
    cursor: Option<Cursor>,
    limit: u32,
) -> Result<Vec<AbuseReport>, RepoError> {
    match cursor {
        Some(cursor) => {
            let cursor_ts = DateTime::from_timestamp_millis(cursor.ts)
                .expect("a cursor ts is a valid Unix-millis timestamp");
            sqlx::query_as!(
                AbuseReport,
                r#"SELECT id, user_id, reporter_contact, job_id, reason, status,
                          resolution, notes, created_at, updated_at, resolved_at,
                          resolved_by
                   FROM abuse_reports
                   WHERE ($1::text IS NULL OR status = $1)
                     AND ($2::uuid IS NULL OR user_id = $2)
                     AND (date_trunc('milliseconds', created_at), id) < ($3, $4)
                   ORDER BY date_trunc('milliseconds', created_at) DESC, id DESC
                   LIMIT $5"#,
                status,
                user_id,
                cursor_ts,
                cursor.id,
                limit as i64,
            )
            .fetch_all(pool)
            .await
            .map_err(RepoError::from_sqlx)
        }
        None => sqlx::query_as!(
            AbuseReport,
            r#"SELECT id, user_id, reporter_contact, job_id, reason, status,
                      resolution, notes, created_at, updated_at, resolved_at,
                      resolved_by
               FROM abuse_reports
               WHERE ($1::text IS NULL OR status = $1)
                 AND ($2::uuid IS NULL OR user_id = $2)
               ORDER BY date_trunc('milliseconds', created_at) DESC, id DESC
               LIMIT $3"#,
            status,
            user_id,
            limit as i64,
        )
        .fetch_all(pool)
        .await
        .map_err(RepoError::from_sqlx),
    }
}

/// Move an open report to reviewing (the admin picks it up).
/// The `WHERE status = 'open'` makes the move conditional:
/// `None` when the report is already reviewing or resolved —
/// the caller re-reads the row and answers the idempotent
/// 200 with its current state. `updated_at` bumps (there is
/// no trigger on this table — the UPDATE stamps it); the
/// resolution columns stay NULL (the report is not resolved).
pub async fn mark_reviewing(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    id: Uuid,
) -> Result<Option<AbuseReport>, RepoError> {
    sqlx::query_as!(
        AbuseReport,
        r#"UPDATE abuse_reports
           SET status = 'reviewing', updated_at = now()
           WHERE id = $1 AND status = 'open'
           RETURNING id, user_id, reporter_contact, job_id, reason, status,
                     resolution, notes, created_at, updated_at, resolved_at,
                     resolved_by"#,
        id,
    )
    .fetch_optional(pool)
    .await
    .map_err(RepoError::from_sqlx)
}

/// Resolve a report (P1-090): status `resolved`, the
/// resolution naming the action (`dismissed` |
/// `job_rejected` | `user_suspended`), the admin's notes when
/// given, and the resolution stamps — `resolved_at` from the
/// DB clock, `resolved_by` the acting admin. The `WHERE
/// status IN ('open', 'reviewing')` makes the resolve
/// conditional: `None` when the report is already resolved
/// (or a concurrent resolve won the race) — the caller
/// re-reads the row and answers the idempotent 200 with its
/// current state. `updated_at` bumps with the resolve (no
/// trigger on this table).
pub async fn resolve(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    id: Uuid,
    resolution: &str,
    notes: Option<&str>,
    resolved_by: Uuid,
) -> Result<Option<AbuseReport>, RepoError> {
    sqlx::query_as!(
        AbuseReport,
        r#"UPDATE abuse_reports
           SET status = 'resolved', resolution = $2, notes = $3,
               resolved_at = now(), resolved_by = $4, updated_at = now()
           WHERE id = $1 AND status IN ('open', 'reviewing')
           RETURNING id, user_id, reporter_contact, job_id, reason, status,
                     resolution, notes, created_at, updated_at, resolved_at,
                     resolved_by"#,
        id,
        resolution,
        notes,
        resolved_by,
    )
    .fetch_optional(pool)
    .await
    .map_err(RepoError::from_sqlx)
}
