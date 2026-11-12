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
//! empty queues. The process also writes a heartbeat row to
//! Postgres every `PF_WORKER_HEARTBEAT_INTERVAL_SECS` (P0-080)
//! and serves its metrics on an internal `/metrics` port.

use detag_core::config::{warn_unknown_feature_flags, Settings};
use detag_core::db::{connect_lazy, db_health, PoolConfig};
use detag_core::metrics::Metrics;
use detag_core::shutdown::{shutdown_signal, SHUTDOWN_TIMEOUT};
use detag_core::telemetry::{init_tracing, shutdown_telemetry};
use detag_worker::heartbeat::{run_heartbeat, WorkerIdentity};
use detag_worker::metrics_server::serve_metrics;
use detag_worker::queue_hooks::{QueueHooks, QUEUES};
use detag_worker::workers::{build_monitor, WorkerConfig};
use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;

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
    // Models (P0-084): verify every model the registry requires before
    // any worker starts consuming jobs. Phase 0 requires none, so this
    // only attests the empty registry — the sha256 check itself is real
    // and runs on the first Phase-2 entry. A failure is a boot error.
    let verified = detag_engine::registry::verify_models(
        std::path::Path::new(&settings.engine_models_dir),
        &detag_engine::registry::required_models(),
    )
    .await
    .unwrap_or_else(|err| {
        eprintln!("detag-worker: model verification failed: {err}");
        std::process::exit(1);
    });
    tracing::info!(
        models_dir = %settings.engine_models_dir,
        verified,
        "models verified"
    );
    // Job/queue metrics hooks (P0-029): the registry is owned by this
    // process; the hooks are the queue implementation's recording
    // surface. The depths start at 0 (no jobs yet); the workers
    // record enqueues/dequeues through the same hooks from P0-075.
    let metrics = Metrics::new();
    let hooks = QueueHooks::new(metrics.queue().clone());
    hooks.reset_depths();
    // Worker identity (P0-080): the heartbeat row's key and the
    // `detag_worker_info` label. Set before anything else reads
    // the worker gauges.
    let identity = WorkerIdentity::current();
    metrics.worker().set_info(&identity.worker_id, identity.pid);
    // Postgres pool (P0-035, E0.4): lazy, like the api's — the worker
    // must not fail boot on a temporarily-down database. No jobs run
    // yet (E0.8); today the pool only proves the database is reachable
    // at startup, backs the retry-event sink (P0-077) and the
    // heartbeat writer (P0-080), and the close below keeps shutdown
    // clean.
    let pool = connect_lazy(&settings.database_url, &PoolConfig::default()).unwrap_or_else(|err| {
        eprintln!("detag-worker: invalid PF_DATABASE_URL: {err}");
        std::process::exit(1);
    });
    let db = db_health(&pool).await;
    // The workers (P0-075): one Redis-backed apalis worker per
    // queue. A dead Redis is a boot error — the worker cannot do
    // its job without the queue — so fail fast naming the var.
    let worker_config = WorkerConfig::from_settings(&settings);
    let (monitor, inflight) = build_monitor(&worker_config, hooks, &pool)
        .await
        .unwrap_or_else(|err| {
            eprintln!("detag-worker: {err}");
            std::process::exit(1);
        });
    // Internal /metrics server (P0-080): its own port on
    // PF_METRICS_HOST (loopback by default, never on a public port).
    // A taken port is a boot error — the gauges are part of the
    // worker's contract, not a nice-to-have.
    let metrics_addr = SocketAddr::from((settings.metrics_host, settings.worker_metrics_port));
    let metrics_listener = tokio::net::TcpListener::bind(metrics_addr)
        .await
        .unwrap_or_else(|err| {
            eprintln!("detag-worker: cannot bind metrics port (PF_WORKER_METRICS_PORT): {err}");
            std::process::exit(1);
        });
    // One signal listener, shared by the monitor, the metrics
    // server and the heartbeat loop (the api's pattern, P0-023):
    // on SIGTERM/SIGINT all three stop together.
    let shutdown = Arc::new(Notify::new());
    {
        let shutdown = Arc::clone(&shutdown);
        tokio::spawn(async move {
            shutdown_signal().await;
            shutdown.notify_waiters();
        });
    }
    let metrics_task = tokio::spawn(serve_metrics(
        metrics.clone(),
        metrics_listener,
        Arc::clone(&shutdown),
    ));
    // The drain log reads the same counter the heartbeat loop
    // writes (P0-080): clone it before the loop takes ownership.
    let drain_inflight = Arc::clone(&inflight);
    let heartbeat_task = tokio::spawn(run_heartbeat(
        pool.clone(),
        identity.clone(),
        inflight,
        metrics.worker().clone(),
        Duration::from_secs(settings.worker_heartbeat_interval_secs),
        Arc::clone(&shutdown),
    ));
    tracing::info!(
        queues = QUEUES.join(","),
        concurrency = settings.worker_concurrency,
        worker_id = %identity.worker_id,
        metrics_port = settings.worker_metrics_port,
        db_ok = db.ok,
        db_detail = db.detail,
        "started (queues: pipeline, cleanup, email, reconciliation)"
    );
    // Graceful drain (P0-081): SIGTERM/SIGINT stops picking up
    // new jobs and the monitor finishes the in-flight ones — but
    // only for `PF_WORKER_DRAIN_TIMEOUT_SECS` (default 30 s).
    // apalis's `run_with_signal` waits unboundedly, so the bound
    // is ours: past it the process exits 1 rather than hanging a
    // deploy, and the un-acked jobs are NOT lost — apalis
    // re-queues them when the visibility timeout expires (NFR-21;
    // the kill-mid-job drill lands in Phase 3). Checkpointing:
    // the Phase-0 handlers are stubs with no partial state to
    // checkpoint, so "finish" is the whole story until real
    // handlers land — a handler that grows resumable state must
    // checkpoint it before returning.
    let wait_shutdown = |shutdown: Arc<Notify>| async move {
        shutdown.notified().await;
        Ok::<(), std::io::Error>(())
    };
    let drain_timeout = Duration::from_secs(settings.worker_drain_timeout_secs);
    let drained_cleanly = match tokio::time::timeout(
        drain_timeout,
        monitor.run_with_signal(wait_shutdown(Arc::clone(&shutdown))),
    )
    .await
    {
        Ok(Ok(())) => {
            tracing::info!(
                in_flight = drain_inflight.load(Ordering::SeqCst),
                "worker drained"
            );
            true
        }
        Ok(Err(err)) => {
            tracing::error!(error = %err, "worker monitor exited with an error");
            false
        }
        Err(_) => {
            tracing::error!(
                in_flight = drain_inflight.load(Ordering::SeqCst),
                drain_timeout_secs = settings.worker_drain_timeout_secs,
                "drain timed out — in-flight jobs requeue via visibility timeout (NFR-21)"
            );
            false
        }
    };
    // The heartbeat loop and the metrics server got the same
    // signal: give them the drain window to finish (the heartbeat
    // deletes its row), then stop waiting — a stuck task must not
    // hang the deploy.
    if tokio::time::timeout(SHUTDOWN_TIMEOUT, async {
        let _ = tokio::join!(metrics_task, heartbeat_task);
    })
    .await
    .is_err()
    {
        tracing::error!("metrics/heartbeat tasks did not stop within the drain window");
    }
    pool.close().await;
    shutdown_telemetry();
    tracing::info!("shutting down");
    // A failed drain (monitor error, or the timeout) exits
    // non-zero: the deploy must know the worker did not stop
    // cleanly, even though no job was lost (NFR-21 re-queues).
    if !drained_cleanly {
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn worker_crate_loads() {
        assert!(!env!("CARGO_PKG_NAME").is_empty());
    }
}
