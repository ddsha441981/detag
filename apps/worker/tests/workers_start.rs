//! P0-075: the workers start and process a versioned envelope.
//!
//! Builds the monitor against the native Redis (this VM) with a
//! unique namespace prefix (test jobs never land in the dev
//! queues), pushes one envelope to the pipeline queue, and
//! asserts the stub handler processed it (the dequeue metric
//! records it) — then shuts the monitor down cleanly.
//!
//! The 5 max retries are the apalis-redis storage default
//! (P0-077 sets the per-queue policy); the envelope here is
//! valid v1, so no retry happens.

use apalis::prelude::*;
use apalis_redis::{Config as RedisConfig, RedisStorage};
use detag_core::db::{connect_lazy, PoolConfig};
use detag_core::metrics::Metrics;
use detag_worker::{
    envelope::JobEnvelope,
    queue_hooks::QueueHooks,
    workers::{build_monitor, WorkerConfig},
};
use std::time::Duration;
use uuid::Uuid;

fn test_config() -> WorkerConfig {
    WorkerConfig {
        redis_url: std::env::var("PF_REDIS_URL")
            .unwrap_or_else(|_| "redis://localhost:6379".to_string()),
        concurrency: 1,
        namespace_prefix: format!("pf:test-queue-{}", Uuid::now_v7()),
        dlq_prefix: format!("pf:test-dlq-{}", Uuid::now_v7()),
        visibility_timeout: Duration::from_secs(300),
        keep_alive: Duration::from_secs(30),
    }
}

#[tokio::test]
async fn monitor_starts_and_processes_an_envelope() {
    let metrics = Metrics::new();
    let hooks = QueueHooks::new(metrics.queue().clone());
    hooks.reset_depths();
    let config = test_config();
    // The retry-event sink's pool (P0-077): lazy, never used here
    // (no handler fails), but the monitor needs it to build.
    let pool = connect_lazy(
        &std::env::var("PF_DATABASE_URL")
            .unwrap_or_else(|_| "postgres://pf:pf@localhost:5432/pf".to_string()),
        &PoolConfig::default(),
    )
    .expect("pool builds");
    let monitor = build_monitor(&config, hooks, &pool)
        .await
        .expect("monitor builds against the native Redis");
    // Run the monitor until the test signals shutdown.
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let monitor_task = tokio::spawn(monitor.run_with_signal(async {
        shutdown_rx.await.ok();
        Ok::<(), std::io::Error>(())
    }));
    // Push one envelope to the pipeline queue (a second storage on
    // the same namespace — the producer side).
    let conn = apalis_redis::connect(config.redis_url.as_str())
        .await
        .expect("Redis connects");
    let mut storage = RedisStorage::new_with_config(
        conn,
        RedisConfig::default().set_namespace(&config.queue_namespace("pipeline")),
    );
    let job_id = Uuid::now_v7();
    storage
        .push(JobEnvelope::new(
            job_id,
            "pipeline",
            serde_json::json!({"probe": true}),
        ))
        .await
        .expect("envelope enqueues");
    // The stub handler records the dequeue; poll the metric until
    // it appears (100 ms poll interval + registration on first
    // poll, so this takes well under a second when healthy).
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if metrics
            .gather()
            .contains("detag_queue_dequeued_total{queue=\"pipeline\"} 1")
        {
            break;
        }
        if tokio::time::Instant::now() > deadline {
            panic!(
                "the envelope was not processed within 10 s:\n{}",
                metrics.gather()
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // Clean shutdown: signal, then the monitor task must finish.
    shutdown_tx.send(()).expect("monitor still running");
    let exited = tokio::time::timeout(Duration::from_secs(10), monitor_task)
        .await
        .expect("monitor shuts down within 10 s");
    exited
        .expect("monitor task joins")
        .expect("monitor exits cleanly");
}
