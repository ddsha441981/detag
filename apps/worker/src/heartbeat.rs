//! Worker heartbeat loop (P0-080): one Postgres row per worker
//! process, upserted every `PF_WORKER_HEARTBEAT_INTERVAL_SECS`
//! (default 15 s) — pid, queues, in-flight count, started_at.
//!
//! The row is the admin view of live workers: a row whose
//! `last_heartbeat_at` is older than ~2 intervals is a dead
//! worker (staleness IS the signal). The write is best-effort —
//! a down Postgres must never fail the job the worker is
//! processing, so a failed beat only warns, counts a failure,
//! and the loop carries on. The same tick also prunes rows older
//! than an hour and pushes the in-flight count into the
//! `detag_worker_inflight_jobs` gauge, so the row and the scrape
//! always agree.
//!
//! On shutdown the loop deletes its own row (best-effort), so a
//! graceful stop does not leave a stale worker in the admin view
//! for an hour. A crash deletes nothing — that is the point: the
//! stale row is how a crash is visible.

use crate::queue_hooks::QUEUES;
use chrono::{DateTime, Utc};
use detag_core::db::DbPool;
use detag_core::metrics::WorkerMetrics;
use detag_core::repos::worker_heartbeats::{
    delete_heartbeat, prune_stale_heartbeats, record_heartbeat, HeartbeatRow, HEARTBEAT_PRUNE_AFTER,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;

/// Identity of this worker process: `{hostname}:{pid}` — the
/// same id as the `detag_worker_info` metric label and the
/// heartbeat row's primary key. Hostname comes from `$HOSTNAME`
/// (the container id in compose); `unknown` when unset.
#[derive(Debug, Clone)]
pub struct WorkerIdentity {
    /// `{hostname}:{pid}`.
    pub worker_id: String,
    /// The OS pid.
    pub pid: u32,
    /// When this process started (frozen in the heartbeat row).
    pub started_at: DateTime<Utc>,
}

impl WorkerIdentity {
    /// Identity of the current process.
    pub fn current() -> Self {
        let pid = std::process::id();
        let hostname = std::env::var("HOSTNAME").unwrap_or_else(|_| "unknown".to_string());
        Self {
            worker_id: format!("{hostname}:{pid}"),
            pid,
            started_at: Utc::now(),
        }
    }
}

/// Run the heartbeat loop until `shutdown` fires: one beat per
/// `interval` (the first beat is immediate, so the row exists
/// from startup), then delete our row and return.
pub async fn run_heartbeat(
    pool: DbPool,
    identity: WorkerIdentity,
    inflight: Arc<AtomicUsize>,
    metrics: WorkerMetrics,
    interval: Duration,
    shutdown: Arc<Notify>,
) {
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // One notified future for the loop's whole life, pinned
    // outside it: a shutdown fired mid-beat must not be lost
    // when the loop re-arms the select (a fresh `notified()`
    // per iteration would miss it and hang until the next tick).
    let notified = shutdown.notified();
    tokio::pin!(notified);
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                beat(&pool, &identity, &inflight, &metrics).await;
            }
            () = &mut notified => {
                if let Err(err) = delete_heartbeat(&pool, &identity.worker_id).await {
                    tracing::warn!(
                        worker_id = %identity.worker_id,
                        error = %err,
                        "heartbeat row not deleted on shutdown (best-effort)"
                    );
                }
                return;
            }
        }
    }
}

/// One beat: upsert the row, push the gauges, prune stale rows.
/// Every step is best-effort — a failure is warned and counted,
/// never fatal.
async fn beat(
    pool: &DbPool,
    identity: &WorkerIdentity,
    inflight: &Arc<AtomicUsize>,
    metrics: &WorkerMetrics,
) {
    let count = inflight.load(Ordering::SeqCst);
    // The gauge describes this process, not the database — it
    // moves even when the row write fails below.
    metrics.set_inflight(count as i64);
    let row = HeartbeatRow {
        worker_id: identity.worker_id.clone(),
        pid: identity.pid as i32,
        queues: QUEUES.iter().map(|queue| queue.to_string()).collect(),
        in_flight: i32::try_from(count).unwrap_or(i32::MAX),
        started_at: identity.started_at,
        last_heartbeat_at: Utc::now(),
    };
    match record_heartbeat(pool, &row).await {
        Ok(()) => metrics.heartbeat_ok(Utc::now().timestamp()),
        Err(err) => {
            metrics.heartbeat_failed();
            tracing::warn!(
                worker_id = %identity.worker_id,
                error = %err,
                "heartbeat write failed (best-effort — the worker keeps running)"
            );
        }
    }
    if let Err(err) = prune_stale_heartbeats(pool, HEARTBEAT_PRUNE_AFTER).await {
        tracing::warn!(error = %err, "stale heartbeat prune failed (best-effort)");
    }
}
