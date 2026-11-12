//! detag-worker — Apalis worker host (bootstrap stub, P0-002;
//! queue wiring P0-075, E0.8).
//!
//! Four Redis-backed apalis queues (pipeline, cleanup, email,
//! reconciliation — see `queue_hooks::QUEUES`), each with
//! `PF_WORKER_CONCURRENCY` workers, running versioned
//! [`JobEnvelope`](envelope::JobEnvelope)s. The kind registry
//! (P0-076) dispatches each envelope to its handler; the
//! handlers are Phase-0 stubs until the features that own them
//! land — nothing enqueues jobs yet, so the workers idle on
//! empty queues.

use detag_core::config::{warn_unknown_feature_flags, Settings};
use detag_core::db::{connect_lazy, db_health, PoolConfig};
use detag_core::metrics::Metrics;
use detag_core::shutdown::shutdown_signal;
use detag_core::telemetry::{init_tracing, shutdown_telemetry};
use detag_worker::queue_hooks::{QueueHooks, QUEUES};
use detag_worker::workers::{build_monitor, WorkerConfig};

#[tokio::main]
async fn main() {
    // Fail boot fast on invalid configuration, naming the bad PF_* var.
    let settings = Settings::load().unwrap_or_else(|err| {
        eprintln!("detag-worker: invalid configuration:\n{err}");
        std::process::exit(1);
    });
    // Tracing first, so the warn below (and everything after) is logged.
    init_tracing(&settings, "detag-worker");
    // Unknown PF_FEATURE_* flags warn (a typo must not silently do nothing).
    warn_unknown_feature_flags();
    // Job/queue metrics hooks (P0-029): the registry is owned by this
    // process; the hooks are the queue implementation's recording
    // surface. The depths start at 0 (no jobs yet); the workers
    // record enqueues/dequeues through the same hooks from P0-075.
    let metrics = Metrics::new();
    let hooks = QueueHooks::new(metrics.queue().clone());
    hooks.reset_depths();
    // Postgres pool (P0-035, E0.4): lazy, like the api's — the worker
    // must not fail boot on a temporarily-down database. No jobs run
    // yet (E0.8); today the pool only proves the database is reachable
    // at startup, and the close below keeps shutdown clean.
    let pool = connect_lazy(&settings.database_url, &PoolConfig::default()).unwrap_or_else(|err| {
        eprintln!("detag-worker: invalid PF_DATABASE_URL: {err}");
        std::process::exit(1);
    });
    let db = db_health(&pool).await;
    // The workers (P0-075): one Redis-backed apalis worker per
    // queue. A dead Redis is a boot error — the worker cannot do
    // its job without the queue — so fail fast naming the var.
    let worker_config = WorkerConfig::from_settings(&settings);
    let monitor = build_monitor(&worker_config, hooks)
        .await
        .unwrap_or_else(|err| {
            eprintln!("detag-worker: {err}");
            std::process::exit(1);
        });
    tracing::info!(
        queues = QUEUES.join(","),
        concurrency = settings.worker_concurrency,
        db_ok = db.ok,
        db_detail = db.detail,
        "started (queues: pipeline, cleanup, email, reconciliation)"
    );
    // Graceful shutdown (P0-023): SIGTERM/SIGINT stops picking up new
    // jobs; the monitor waits for in-flight jobs, then the pool
    // closes. The drain semantics (timeout, checkpointing) land in
    // P0-081 — with stub handlers the in-flight set is empty.
    if let Err(err) = monitor
        .run_with_signal(async {
            shutdown_signal().await;
            Ok::<(), std::io::Error>(())
        })
        .await
    {
        tracing::error!(error = %err, "worker monitor exited with an error");
    }
    pool.close().await;
    shutdown_telemetry();
    tracing::info!("shutting down");
}

#[cfg(test)]
mod tests {
    #[test]
    fn worker_crate_loads() {
        assert!(!env!("CARGO_PKG_NAME").is_empty());
    }
}
