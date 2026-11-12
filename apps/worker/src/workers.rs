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
//! unknown kind. The retry policy (P0-077), DLQ (P0-078) and
//! graceful drain (P0-081) all extend this flow — nothing
//! enqueues jobs in Phase 0, so it never runs in production
//! today.
//!
//! Storage defaults in force (apalis-redis `Config::default`):
//! 100 ms poll, 10-job buffer, 5 max retries, 30 s keep-alive,
//! 30 s scheduled-enqueue sweep, 300 s orphan requeue. The
//! retry count is the storage default — P0-077 sets the
//! per-queue policy.

use crate::envelope::JobEnvelope;
use crate::queue_hooks::{QueueHooks, QUEUES};
use crate::registry::{DispatchOutcome, JobRegistry};
use apalis::prelude::*;
use apalis_redis::{Config as RedisConfig, RedisStorage};
use detag_core::config::Settings;

/// Redis key prefix for the apalis queues
/// (`pf:queue:{queue}:{suffix}`).
pub const QUEUE_NAMESPACE_PREFIX: &str = "pf:queue";

/// What [`build_monitor`] needs. Built from [`Settings`] in
/// production (`from_settings`); tests pass their own values
/// (notably a unique `namespace_prefix`, so test jobs never
/// land in the dev queues).
#[derive(Debug, Clone)]
pub struct WorkerConfig {
    /// Redis URL (PF_REDIS_URL).
    pub redis_url: String,
    /// Workers per queue (PF_WORKER_CONCURRENCY).
    pub concurrency: usize,
    /// Namespace prefix for the apalis queues.
    pub namespace_prefix: String,
}

impl WorkerConfig {
    /// Production config from loaded settings.
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            redis_url: settings.redis_url.clone(),
            concurrency: settings.worker_concurrency,
            namespace_prefix: QUEUE_NAMESPACE_PREFIX.to_string(),
        }
    }

    /// The apalis namespace for one queue
    /// (`pf:queue:pipeline`, ...).
    pub fn queue_namespace(&self, queue: &str) -> String {
        format!("{}:{queue}", self.namespace_prefix)
    }
}

/// Per-worker context, injected into every handler call: which
/// queue this worker serves, the metrics hooks, and the kind
/// registry that dispatches the envelope.
#[derive(Clone, Debug)]
pub struct WorkerContext {
    /// The queue this worker serves (`pipeline`, ...).
    pub queue: &'static str,
    /// The process's metrics hooks.
    pub hooks: QueueHooks,
    /// Kind → handler map (P0-076).
    pub registry: JobRegistry,
}

/// Building the monitor failed. The Redis URL is never echoed —
/// it can carry a password in production.
#[derive(Debug, thiserror::Error)]
pub enum WorkerError {
    /// The Redis connection could not be established.
    #[error("cannot connect to Redis (PF_REDIS_URL)")]
    RedisConnect(#[from] apalis_redis::RedisError),
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
pub async fn build_monitor(
    config: &WorkerConfig,
    hooks: QueueHooks,
) -> Result<Monitor, WorkerError> {
    let conn = apalis_redis::connect(config.redis_url.as_str()).await?;
    let mut monitor = Monitor::new();
    for queue in QUEUES {
        let storage = RedisStorage::new_with_config(
            conn.clone(),
            RedisConfig::default().set_namespace(&config.queue_namespace(queue)),
        );
        for replica in 0..config.concurrency {
            let ctx = WorkerContext {
                queue,
                hooks: hooks.clone(),
                registry: JobRegistry::new(),
            };
            let worker = WorkerBuilder::new(format!("{queue}-{replica}"))
                .data(ctx)
                .backend(storage.clone())
                .build_fn(handle_envelope);
            monitor = monitor.register(worker);
        }
    }
    Ok(monitor)
}

/// Handler: version-check, hash-check, record the dequeue,
/// dispatch by kind. A handler error propagates so apalis
/// retries it under the P0-077 policy. A job this build cannot
/// trust — unknown version, or a params hash that does not match
/// the payload — is never executed and never retried: it is
/// quarantined (counted + logged) and acknowledged. Retrying
/// the same bytes cannot fix either failure.
async fn handle_envelope(job: JobEnvelope, ctx: Data<WorkerContext>) -> Result<(), Error> {
    if let Err(err) = job.check_version() {
        ctx.hooks.quarantined(ctx.queue);
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
