//! Job events repository (P0-077, E0.8). `job_events` is the
//! append-only audit trail for a job (migration 002): one row per
//! event, `type` is `stage | progress | note | state_change`.
//!
//! The only writer in Phase 0 is the worker retry policy: every
//! retry decision lands here as a `state_change` event with the
//! attempt, the backoff delay and the error that caused it, so a
//! job's retry history is replayable from the table (and visible
//! on the job's pubsub channel in later phases). The write is
//! best-effort from the caller's side — a missing jobs row (an
//! FK violation) must never fail the job itself; see the worker
//! `PostgresRetryEventSink`.

use super::error::RepoError;
use std::time::Duration;
use uuid::Uuid;

/// The payload of a retry event. `event` is always `"retry"`;
/// the rest is the decision the policy made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryEventRow {
    /// The attempt that just failed (0-based).
    pub attempt: i32,
    /// The backoff delay before the next attempt, in milliseconds.
    pub delay_ms: i64,
    /// The error that caused the retry (handler error text).
    pub error: String,
}

/// Append a retry event for `job_id`. Returns the event row's id.
/// Fails with `RepoError::ForeignKeyViolation` when no jobs row
/// exists for `job_id` (the caller treats that as best-effort).
pub async fn record_retry_event(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    job_id: Uuid,
    event: &RetryEventRow,
) -> Result<Uuid, RepoError> {
    let payload = serde_json::json!({
        "event": "retry",
        "attempt": event.attempt,
        "delay_ms": event.delay_ms,
        "error": event.error,
    });
    let row = sqlx::query!(
        r#"INSERT INTO job_events (job_id, type, payload)
           VALUES ($1, 'state_change', $2)
           RETURNING id"#,
        job_id,
        payload,
    )
    .fetch_one(pool)
    .await
    .map_err(RepoError::from_sqlx)?;
    Ok(row.id)
}

/// Read back the retry events for `job_id` (newest last), as
/// stored — the payload round-trip check for tests.
pub async fn retry_events_for_job(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    job_id: Uuid,
) -> Result<Vec<serde_json::Value>, RepoError> {
    let rows = sqlx::query!(
        r#"SELECT payload FROM job_events
           WHERE job_id = $1 AND type = 'state_change'
             AND payload ->> 'event' = 'retry'
           ORDER BY at ASC"#,
        job_id,
    )
    .fetch_all(pool)
    .await
    .map_err(RepoError::from_sqlx)?;
    Ok(rows.into_iter().map(|row| row.payload).collect())
}

/// How long a retry event's delay is, in whole milliseconds —
/// the unit the `delay_ms` payload field carries.
pub fn delay_ms(delay: Duration) -> i64 {
    delay.as_millis().try_into().unwrap_or(i64::MAX)
}
