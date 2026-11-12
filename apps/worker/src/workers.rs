//! Apalis wiring (P0-075, E0.8): Redis-backed workers for the
//! four queues.
//!
//! One [`Monitor`] holds one apalis worker per queue
//! (`pipeline`, `cleanup`, `email`, `reconciliation` — see
//! [`QUEUES`](crate::queue_hooks::QUEUES)), each with
//! `PF_WORKER_CONCURRENCY` replicas
//! ([`Monitor::register_with_count`]). Every worker reads from
//! its own [`RedisStorage`](apalis_redis::RedisStorage), scoped
//! to the queue by namespace: keys are
//! `pf:queue:{queue}:{suffix}` (e.g. `pf:queue:pipeline:active`),
//! following the `pf:` prefix convention of the core redis keys
//! module — apalis builds its own keys from the namespace, so
//! the prefix lives in exactly one place
//! ([`QUEUE_NAMESPACE_PREFIX`]).
//!
//! The job type is the versioned [`JobEnvelope`]. The handler
//! below version-checks, records the dequeue, and hands the
//! envelope to the [`JobRegistry`](crate::registry::JobRegistry),
//! which maps the kind to a handler (P0-076) or quarantines an
//! unknown kind. The handler is wrapped in the P0-077 retry
//! layer (per-queue policy, backoff, abort-to-dead). Anything
//! quarantined — or dead after retries — is filed in the DLQ
//! (P0-078) with its payload snapshot. Graceful drain (P0-081)
//! extends this flow — nothing enqueues jobs in Phase 0, so it
//! never runs in production today.
//!
//! Storage defaults in force (apalis-redis `Config::default`,
//! now made explicit in P0-079 — see [`apalis_config`]):
//! 100 ms poll, 10-job buffer, 5 max retries, 30 s scheduled-
//! enqueue sweep. The retry count is the storage default —
//! P0-077 sets the per-queue policy. The two timeouts that
//! decide NFR-21 (kill -9 loses no job) are
//! `PF_QUEUE_KEEP_ALIVE_SECS` (default 30) and
//! `PF_QUEUE_VISIBILITY_TIMEOUT_SECS` (default 300): a live
//! worker re-registers its consumer every keep-alive, so a job
//! running longer than the visibility timeout on a LIVE worker
//! is never re-queued; a dead worker's keep-alives stop, and
//! the orphan sweep — which every live worker runs every poll
//! interval — moves its in-flight jobs back to the active list
//! once their consumer is older than the visibility timeout.
//! If every worker is dead, nothing sweeps until one restarts:
//! that is what `just queue:requeue-stuck` (P0-079) is for —
//! the same sweep, run on demand by an operator.

use crate::dlq::{DeadLetterSink, RedisDeadLetterSink, DLQ_PREFIX};
use crate::envelope::JobEnvelope;
use crate::queue_hooks::{QueueHooks, QUEUES};
use crate::registry::{DispatchOutcome, JobRegistry};
use crate::retry::{PostgresRetryEventSink, RetryLayer, RetryPolicy};
use apalis::prelude::*;
use apalis_redis::{Config as RedisConfig, RedisStorage};
use detag_core::config::Settings;
use detag_core::db::DbPool;
use detag_core::redis::{create_pool, RedisPoolConfig};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Redis key prefix for the apalis queues
/// (`pf:queue:{queue}:{suffix}`).
pub const QUEUE_NAMESPACE_PREFIX: &str = "pf:queue";

/// What [`build_monitor`] needs. Built from [`Settings`] in
/// production (`from_settings`); tests pass their own values
/// (notably a unique `namespace_prefix`, so test jobs never
/// land in the dev queues, and a unique `dlq_prefix`, so test
/// letters never land in the dev DLQ).
#[derive(Debug, Clone)]
pub struct WorkerConfig {
    /// Redis URL (PF_REDIS_URL).
    pub redis_url: String,
    /// Workers per queue (PF_WORKER_CONCURRENCY).
    pub concurrency: usize,
    /// Namespace prefix for the apalis queues.
    pub namespace_prefix: String,
    /// Key prefix for the dead-letter sets (`pf:dlq` in prod).
    pub dlq_prefix: String,
    /// Visibility timeout (PF_QUEUE_VISIBILITY_TIMEOUT_SECS):
    /// a dead worker's in-flight jobs are re-queued after this
    /// long without a keep-alive (NFR-21, P0-079).
    pub visibility_timeout: Duration,
    /// Worker heartbeat interval (PF_QUEUE_KEEP_ALIVE_SECS).
    pub keep_alive: Duration,
}

impl WorkerConfig {
    /// Production config from loaded settings.
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            redis_url: settings.redis_url.clone(),
            concurrency: settings.worker_concurrency,
            namespace_prefix: QUEUE_NAMESPACE_PREFIX.to_string(),
            dlq_prefix: DLQ_PREFIX.to_string(),
            visibility_timeout: Duration::from_secs(settings.queue_visibility_timeout_secs),
            keep_alive: Duration::from_secs(settings.queue_keep_alive_secs),
        }
    }

    /// The apalis namespace for one queue
    /// (`pf:queue:pipeline`, ...).
    pub fn queue_namespace(&self, queue: &str) -> String {
        format!("{}:{queue}", self.namespace_prefix)
    }
}

/// The apalis storage config for one queue: the namespace from
/// [`WorkerConfig::queue_namespace`] plus the P0-079 timeouts
/// (keep-alive + visibility) from the config — never the
/// apalis defaults left implicit. Kept separate from
/// [`build_monitor`] so the values are unit-testable.
pub fn apalis_config(config: &WorkerConfig, queue: &str) -> RedisConfig {
    RedisConfig::default()
        .set_namespace(&config.queue_namespace(queue))
        .set_keep_alive(config.keep_alive)
        .set_reenqueue_orphaned_after(config.visibility_timeout)
}

/// Per-worker context, injected into every handler call: which
/// queue this worker serves, the metrics hooks, the kind
/// registry that dispatches the envelope, and the dead-letter
/// sink that records quarantines.
#[derive(Clone, Debug)]
pub struct WorkerContext {
    /// The queue this worker serves (`pipeline`, ...).
    pub queue: &'static str,
    /// The process's metrics hooks.
    pub hooks: QueueHooks,
    /// Kind → handler map (P0-076).
    pub registry: JobRegistry,
    /// Dead-letter sink (P0-078).
    pub dlq: Arc<dyn DeadLetterSink + Send + Sync>,
    /// Jobs inside a handler right now, across all workers in
    /// this process (P0-080): the heartbeat row and the
    /// `detag_worker_inflight_jobs` gauge read this counter.
    pub inflight: Arc<AtomicUsize>,
}

/// RAII guard: counts one job in flight while it lives.
/// Created at the top of [`handle_envelope`] so every exit path
/// — success, quarantine early-return, handler error — decrements.
struct InflightGuard {
    inflight: Arc<AtomicUsize>,
}

impl InflightGuard {
    fn new(inflight: &Arc<AtomicUsize>) -> Self {
        inflight.fetch_add(1, Ordering::SeqCst);
        Self {
            inflight: Arc::clone(inflight),
        }
    }
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        self.inflight.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Building the monitor failed. The Redis URL is never echoed —
/// it can carry a password in production.
#[derive(Debug, thiserror::Error)]
pub enum WorkerError {
    /// The Redis connection could not be established.
    #[error("cannot connect to Redis (PF_REDIS_URL)")]
    RedisConnect(#[from] apalis_redis::RedisError),
    /// The DLQ pool could not be built (bad PF_REDIS_URL).
    #[error("cannot build the DLQ pool (PF_REDIS_URL)")]
    DlqPool(#[from] detag_core::redis::RedisPoolError),
}

/// Build the monitor: connect once to Redis, then one
/// Redis-backed worker per queue at `config.concurrency`
/// replicas. The caller runs it (see `main.rs`:
/// `run_with_signal(shutdown_signal)`).
///
/// Concurrency in apalis 0.6 is per worker actor: a single
/// worker already runs jobs concurrently (`CallAllUnordered`),
/// and `register_with_count` (the old way to add replicas) is
/// deprecated — so each replica is built fresh with its own
/// name (`{queue}-{i}`), which is also its Redis consumer id.
/// The storage is cloned per replica (it shares the one
/// connection manager).
///
/// Each worker's handler is wrapped in the P0-077 retry layer
/// (per-queue policy + `job_events` sink from `pool`). The DLQ
/// pool (P0-078) is built once here and shared by every worker.
///
/// Returns the monitor plus the shared in-flight counter
/// (P0-080): every handler increments it on entry and decrements
/// on exit, so the heartbeat loop and the gauges read one number
/// for the whole process.
pub async fn build_monitor(
    config: &WorkerConfig,
    hooks: QueueHooks,
    pool: &DbPool,
) -> Result<(Monitor, Arc<AtomicUsize>), WorkerError> {
    let conn = apalis_redis::connect(config.redis_url.as_str()).await?;
    let sink = PostgresRetryEventSink::new(pool.clone());
    let dlq_pool = create_pool(&config.redis_url, &RedisPoolConfig::default())?;
    let dlq: Arc<dyn DeadLetterSink + Send + Sync> = Arc::new(RedisDeadLetterSink::new(
        dlq_pool,
        config.dlq_prefix.clone(),
    ));
    let inflight = Arc::new(AtomicUsize::new(0));
    let mut monitor = Monitor::new();
    for queue in QUEUES {
        let storage = RedisStorage::new_with_config(conn.clone(), apalis_config(config, queue));
        for replica in 0..config.concurrency {
            let ctx = WorkerContext {
                queue,
                hooks: hooks.clone(),
                registry: JobRegistry::new(),
                dlq: dlq.clone(),
                inflight: inflight.clone(),
            };
            let worker = WorkerBuilder::new(format!("{queue}-{replica}"))
                .data(ctx)
                .layer(RetryLayer::new(
                    RetryPolicy::for_queue(queue),
                    sink.clone(),
                    hooks.clone(),
                    dlq.clone(),
                    queue,
                ))
                .backend(storage.clone())
                .build_fn(handle_envelope);
            monitor = monitor.register(worker);
        }
    }
    Ok((monitor, inflight))
}

/// Handler: version-check, hash-check, record the dequeue,
/// dispatch by kind. A handler error propagates so the P0-077
/// retry layer retries it. A job this build cannot trust —
/// unknown version, or a params hash that does not match the
/// payload — is never executed and never retried: it is
/// quarantined (counted + logged + filed in the DLQ with its
/// payload snapshot) and acknowledged. Retrying the same bytes
/// cannot fix either failure.
async fn handle_envelope(job: JobEnvelope, ctx: Data<WorkerContext>) -> Result<(), Error> {
    // Count this job in flight for the whole handler — every exit
    // path below (quarantine early-return, handler error, success)
    // drops the guard and decrements.
    let _inflight = InflightGuard::new(&ctx.inflight);
    if let Err(err) = job.check_version() {
        ctx.hooks.quarantined(ctx.queue);
        ctx.dlq
            .quarantine(
                &job,
                ctx.queue,
                &format!("unsupported envelope version {}", job.version),
                None,
            )
            .await;
        tracing::error!(
            job_id = %job.job_id,
            kind = %job.kind,
            queue = ctx.queue,
            error = %err,
            "envelope quarantined (unsupported version)"
        );
        return Ok(());
    }
    if !job.verify_params_hash() {
        ctx.hooks.quarantined(ctx.queue);
        ctx.dlq
            .quarantine(&job, ctx.queue, "params_hash mismatch", None)
            .await;
        tracing::error!(
            job_id = %job.job_id,
            kind = %job.kind,
            queue = ctx.queue,
            "envelope quarantined (params_hash mismatch — params tampered with in transit)"
        );
        return Ok(());
    }
    ctx.hooks.dequeued(ctx.queue);
    match ctx.registry.dispatch(&job, &ctx).await? {
        DispatchOutcome::Handled { kind } => {
            tracing::info!(
                job_id = %job.job_id,
                kind = %kind.as_str(),
                queue = ctx.queue,
                "envelope handled"
            );
        }
        DispatchOutcome::Quarantined { kind } => {
            ctx.dlq
                .quarantine(&job, ctx.queue, &format!("unknown kind {kind}"), None)
                .await;
            tracing::info!(
                job_id = %job.job_id,
                kind = %kind,
                queue = ctx.queue,
                "envelope quarantined (unknown kind)"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// P0-080: the guard counts one job in flight while it lives
    /// and decrements on drop — the heartbeat row and the gauge
    /// read the same counter the handler maintains.
    #[test]
    fn inflight_guard_counts_while_it_lives() {
        let inflight = Arc::new(AtomicUsize::new(0));
        {
            let _first = InflightGuard::new(&inflight);
            assert_eq!(inflight.load(Ordering::SeqCst), 1);
            let _second = InflightGuard::new(&inflight);
            assert_eq!(inflight.load(Ordering::SeqCst), 2);
        }
        assert_eq!(
            inflight.load(Ordering::SeqCst),
            0,
            "dropped guards decrement"
        );
    }

    /// P0-079: the storage config carries the configured
    /// keep-alive and visibility timeout — never the apalis
    /// defaults left implicit.
    #[test]
    fn apalis_config_carries_the_configured_timeouts() {
        let config = WorkerConfig {
            redis_url: "redis://localhost:6379".to_string(),
            concurrency: 1,
            namespace_prefix: "pf:test-queue".to_string(),
            dlq_prefix: "pf:test-dlq".to_string(),
            visibility_timeout: Duration::from_secs(600),
            keep_alive: Duration::from_secs(15),
        };
        let apalis = apalis_config(&config, "pipeline");
        assert_eq!(apalis.get_namespace(), "pf:test-queue:pipeline");
        assert_eq!(*apalis.get_keep_alive(), Duration::from_secs(15));
        assert_eq!(apalis.reenqueue_orphaned_after(), Duration::from_secs(600));
    }
}
