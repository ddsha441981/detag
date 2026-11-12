//! Worker heartbeat repository (P0-080, E0.8). `worker_heartbeats`
//! is process state, not domain data: one row per worker process,
//! upserted every heartbeat tick (pid, queues, in-flight count,
//! started_at). A row whose `last_heartbeat_at` is older than
//! ~2 heartbeat intervals is a dead worker — staleness IS the
//! signal, so a crash deletes nothing (a graceful shutdown
//! deletes its own row; the writer prunes rows older than 1 h).
//!
//! Writes are best-effort from the caller's side — a down
//! Postgres must never fail the job the worker is processing;
//! see the worker `heartbeat` loop.

use super::error::RepoError;
use chrono::{DateTime, Utc};
use std::time::Duration;

/// One heartbeat row, as stored.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct HeartbeatRow {
    /// `{hostname}:{pid}` — the same id as the
    /// `detag_worker_info` metric label.
    pub worker_id: String,
    /// The OS pid.
    pub pid: i32,
    /// The queues this process serves.
    pub queues: Vec<String>,
    /// Jobs inside a handler right now (fetched, not yet acked).
    pub in_flight: i32,
    /// When this process started (never changes after the first
    /// insert — the upsert keeps the original value).
    pub started_at: DateTime<Utc>,
    /// When this row was last upserted.
    pub last_heartbeat_at: DateTime<Utc>,
}

/// How stale a heartbeat row must be before the writer prunes
/// it: an hour of silence is a dead worker, not a slow tick.
pub const HEARTBEAT_PRUNE_AFTER: Duration = Duration::from_secs(3600);

/// Upsert this worker's heartbeat row: insert on the first tick,
/// then update `pid`, `queues`, `in_flight` and
/// `last_heartbeat_at` on every tick. `started_at` keeps the
/// original value — a restart is a new `worker_id` only when the
/// pid changes, so a restarted process with a reused pid would
/// otherwise inherit the old start time; the pid + start pair is
/// what the admin view reads, and a stale row from a previous
/// life is pruned within the hour.
pub async fn record_heartbeat(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    row: &HeartbeatRow,
) -> Result<(), RepoError> {
    sqlx::query!(
        r#"INSERT INTO worker_heartbeats
               (worker_id, pid, queues, in_flight, started_at, last_heartbeat_at)
           VALUES ($1, $2, $3, $4, $5, now())
           ON CONFLICT (worker_id) DO UPDATE
               SET pid = EXCLUDED.pid,
                   queues = EXCLUDED.queues,
                   in_flight = EXCLUDED.in_flight,
                   last_heartbeat_at = now()"#,
        row.worker_id,
        row.pid,
        &row.queues,
        row.in_flight,
        row.started_at,
    )
    .execute(pool)
    .await
    .map_err(RepoError::from_sqlx)?;
    Ok(())
}

/// All heartbeat rows, most recently heartbeating first — the
/// admin view of live workers (and, below the live ones, the
/// recently dead).
pub async fn list_heartbeats(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
) -> Result<Vec<HeartbeatRow>, RepoError> {
    sqlx::query_as!(
        HeartbeatRow,
        r#"SELECT worker_id, pid, queues, in_flight, started_at, last_heartbeat_at
           FROM worker_heartbeats
           ORDER BY last_heartbeat_at DESC"#
    )
    .fetch_all(pool)
    .await
    .map_err(RepoError::from_sqlx)
}

/// Delete heartbeat rows older than `older_than` (cutoff =
/// now − `older_than`). Returns how many rows were pruned.
pub async fn prune_stale_heartbeats(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    older_than: Duration,
) -> Result<u64, RepoError> {
    let cutoff =
        Utc::now() - chrono::Duration::from_std(older_than).unwrap_or(chrono::Duration::MAX);
    let result = sqlx::query!(
        r#"DELETE FROM worker_heartbeats
           WHERE last_heartbeat_at < $1"#,
        cutoff,
    )
    .execute(pool)
    .await
    .map_err(RepoError::from_sqlx)?;
    Ok(result.rows_affected())
}

/// Delete one worker's heartbeat row (graceful shutdown).
pub async fn delete_heartbeat(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    worker_id: &str,
) -> Result<(), RepoError> {
    sqlx::query!(
        r#"DELETE FROM worker_heartbeats WHERE worker_id = $1"#,
        worker_id,
    )
    .execute(pool)
    .await
    .map_err(RepoError::from_sqlx)?;
    Ok(())
}
