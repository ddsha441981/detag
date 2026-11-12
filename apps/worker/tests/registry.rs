//! P0-076: an unknown-kind envelope is quarantined safely.
//!
//! Builds the monitor against the native Redis (this VM) with a
//! unique namespace prefix (test jobs never land in the dev
//! queues), pushes one envelope with an unknown kind to the
//! pipeline queue, and asserts it is quarantined — the
//! quarantine counter records it, no handler runs, the job is
//! acknowledged (no retry storm: the counter stays at 1) —
//! and the worker is still alive afterwards: a valid `ping`
//! envelope pushed next is processed. Then shuts the monitor
//! down cleanly.
//!
//! The DLQ persistence (payload snapshot) lands in P0-078;
//! this ticket proves the quarantine decision + the safety
//! properties (never executed, never retried, worker survives).

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
        namespace_prefix: format!("pf:test-registry-{}", Uuid::now_v7()),
        dlq_prefix: format!("pf:test-dlq-{}", Uuid::now_v7()),
    }
}

async fn push_envelope(config: &WorkerConfig, envelope: JobEnvelope) {
    let conn = apalis_redis::connect(config.redis_url.as_str())
        .await
        .expect("Redis connects");
    let mut storage = RedisStorage::new_with_config(
        conn,
        RedisConfig::default().set_namespace(&config.queue_namespace("pipeline")),
    );
    storage.push(envelope).await.expect("envelope enqueues");
}

/// Poll the gathered metrics until `needle` appears (panics after 10 s).
async fn wait_for_metric(metrics: &Metrics, needle: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if metrics.gather().contains(needle) {
            return;
        }
        if tokio::time::Instant::now() > deadline {
            panic!("metric {needle} never appeared:\n{}", metrics.gather());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn unknown_kind_quarantines_and_worker_survives() {
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
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let monitor_task = tokio::spawn(monitor.run_with_signal(async {
        shutdown_rx.await.ok();
        Ok::<(), std::io::Error>(())
    }));

    // 1. The poison: an unknown kind. It must be quarantined
    // (counter 1), dequeued once, and never retried (the
    // quarantine counter must stay at 1 after the settle window).
    push_envelope(
        &config,
        JobEnvelope::new(Uuid::now_v7(), "explode", serde_json::json!({"boom": true})),
    )
    .await;
    wait_for_metric(
        &metrics,
        "detag_queue_quarantined_total{queue=\"pipeline\"} 1",
    )
    .await;
    wait_for_metric(&metrics, "detag_queue_dequeued_total{queue=\"pipeline\"} 1").await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(
        metrics
            .gather()
            .contains("detag_queue_quarantined_total{queue=\"pipeline\"} 1"),
        "the poison must not be retried (quarantine stays at 1):\n{}",
        metrics.gather()
    );

    // 2. The worker must still be alive: a valid ping pushed
    // next is processed (dequeue count moves to 2).
    push_envelope(
        &config,
        JobEnvelope::new(Uuid::now_v7(), "ping", serde_json::json!({"probe": true})),
    )
    .await;
    wait_for_metric(&metrics, "detag_queue_dequeued_total{queue=\"pipeline\"} 2").await;

    // Clean shutdown: signal, then the monitor task must finish.
    shutdown_tx.send(()).expect("monitor still running");
    let exited = tokio::time::timeout(Duration::from_secs(10), monitor_task)
        .await
        .expect("monitor shuts down within 10 s");
    exited
        .expect("monitor task joins")
        .expect("monitor exits cleanly");
}
