//! Requeue-stuck sweep (P0-079, NFR-21): the operator side of
//! the visibility timeout.
//!
//! The workers' own heartbeat already sweeps orphans — every
//! live worker runs `reenqueue_orphaned` every poll interval
//! (see `workers.rs`). This sweep exists for the case the
//! heartbeat cannot cover: every worker dead at once. Then no
//! keep-alive refreshes, no sweep runs, and a restarted worker
//! only re-queues orphans on its own first heartbeat tick —
//! the operator does not have to wait for that, and does not
//! have to restart a worker at all to unstick a queue: run
//! `just queue:requeue-stuck` and the jobs are back on the
//! active list immediately.
//!
//! The sweep is exactly the storage's own orphan call —
//! `RedisStorage::reenqueue_orphaned` with
//! `dead_since = now - older_than` — the same Lua script the
//! heartbeat runs, not a reimplementation: a consumer whose
//! last keep-alive is older than `dead_since` has its in-flight
//! jobs moved back to the active list (and is unregistered
//! once drained). A live worker's consumer is fresh, so its
//! jobs are never touched — the sweep cannot steal a job that
//! is still running.

use crate::envelope::JobEnvelope;
use apalis_redis::{Config as RedisConfig, RedisStorage};
use chrono::{DateTime, Utc};
use std::time::Duration;

/// The sweep failed. The Redis URL is never echoed — it can
/// carry a password in production.
#[derive(Debug, thiserror::Error)]
pub enum RequeueError {
    /// The Redis connection could not be established.
    #[error("cannot connect to Redis (PF_REDIS_URL)")]
    RedisConnect(#[from] apalis_redis::RedisError),
}

/// Re-queue orphaned jobs for `queues`: jobs whose worker's
/// last keep-alive is older than `older_than` (default: the
/// visibility timeout, `PF_QUEUE_VISIBILITY_TIMEOUT_SECS`).
/// At most `limit` jobs per queue per run (the heartbeat's own
/// sweep uses `buffer_size * 10` = 100; the same default
/// applies here — re-run to drain a larger backlog).
///
/// Returns one `(queue, requeued)` pair per queue, in the
/// order given. A queue with no orphans reports 0 — that is
/// the normal case, not an error.
pub async fn requeue_stuck(
    redis_url: &str,
    queues: &[&str],
    namespace_prefix: &str,
    older_than: Duration,
    limit: i32,
) -> Result<Vec<(String, usize)>, RequeueError> {
    // A duration chrono cannot represent (more than ~262k years)
    // means "everything older than forever" — sweep all orphans.
    let dead_since: DateTime<Utc> =
        Utc::now() - chrono::Duration::from_std(older_than).unwrap_or(chrono::Duration::MAX);
    let mut results = Vec::with_capacity(queues.len());
    for queue in queues {
        let conn = apalis_redis::connect(redis_url).await?;
        let mut storage = RedisStorage::<JobEnvelope>::new_with_config(
            conn,
            RedisConfig::default().set_namespace(&format!("{namespace_prefix}:{queue}")),
        );
        let requeued = storage.reenqueue_orphaned(limit, dead_since).await?;
        tracing::info!(
            queue = queue,
            requeued = requeued,
            older_than_secs = older_than.as_secs(),
            "requeue-stuck sweep done"
        );
        results.push(((*queue).to_string(), requeued));
    }
    Ok(results)
}
