//! P0-078: poison is quarantined into the DLQ with its payload
//! snapshot, and the listing reads it back.
//!
//! Builds the monitor against the native Redis (this VM) with a
//! unique namespace prefix AND a unique DLQ prefix (test letters
//! never land in the dev queues or the dev DLQ), pushes an
//! unknown-kind envelope and a tampered envelope to the pipeline
//! queue, and asserts both land in the DLQ — with the job id,
//! the kind, the reason, and the exact payload that was pushed
//! — and a valid `ping` pushed after is still processed (the
//! worker survives the poison). Then shuts the monitor down
//! cleanly.

use apalis::prelude::*;
use apalis_redis::{Config as RedisConfig, RedisStorage};
use detag_core::db::{connect_lazy, PoolConfig};
use detag_core::metrics::Metrics;
use detag_core::redis::{create_pool, RedisPoolConfig};
use detag_worker::{
    dlq::{DeadLetter, RedisDeadLetterSink},
    envelope::JobEnvelope,
    queue_hooks::QueueHooks,
    workers::{build_monitor, WorkerConfig},
};
use std::time::Duration;
use uuid::Uuid;

fn test_config() -> (WorkerConfig, String) {
    let dlq_prefix = format!("pf:test-dlq-{}", Uuid::now_v7());
    (
        WorkerConfig {
            redis_url: std::env::var("PF_REDIS_URL")
                .unwrap_or_else(|_| "redis://localhost:6379".to_string()),
            concurrency: 1,
            namespace_prefix: format!("pf:test-dlq-queue-{}", Uuid::now_v7()),
            dlq_prefix: dlq_prefix.clone(),
            visibility_timeout: Duration::from_secs(300),
            keep_alive: Duration::from_secs(30),
        },
        dlq_prefix,
    )
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

/// Poll the DLQ until it holds `count` letters (panics after 10 s).
async fn wait_for_letters(sink: &RedisDeadLetterSink, count: usize) -> Vec<DeadLetter> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let letters = sink.list_dead("pipeline", 10).await.expect("DLQ lists");
        if letters.len() >= count {
            return letters;
        }
        if tokio::time::Instant::now() > deadline {
            panic!("DLQ never reached {count} letters, has {}", letters.len());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn poison_is_quarantined_with_its_payload_snapshot() {
    let metrics = Metrics::new();
    let hooks = QueueHooks::new(metrics.queue().clone());
    hooks.reset_depths();
    let (config, dlq_prefix) = test_config();
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
    // The DLQ reader: same prefix the monitor writes under.
    let redis_pool =
        create_pool(&config.redis_url, &RedisPoolConfig::default()).expect("DLQ pool builds");
    let dlq = RedisDeadLetterSink::new(redis_pool, dlq_prefix);

    // 1. The poison: an unknown kind with a distinctive payload.
    let poison_id = Uuid::now_v7();
    push_envelope(
        &config,
        JobEnvelope::new(
            poison_id,
            "explode",
            serde_json::json!({"image": "s3://detag-dev/poison.png"}),
        ),
    )
    .await;
    // 2. A tampered envelope: valid version, wrong hash.
    let tampered_id = Uuid::now_v7();
    let mut tampered = JobEnvelope::new(
        tampered_id,
        "pipeline",
        serde_json::json!({"image": "s3://detag-dev/real.png"}),
    );
    tampered.payload = serde_json::json!({"image": "s3://detag-dev/swapped.png"});
    push_envelope(&config, tampered).await;

    let letters = wait_for_letters(&dlq, 2).await;
    // Newest first: the tampered letter sorts above the poison
    // only when its died_at is later — same-second ties are
    // possible, so find each letter by job id instead of order.
    let poison = letters
        .iter()
        .find(|l| l.job_id == poison_id)
        .expect("poison letter present");
    assert_eq!(poison.kind, "explode");
    assert_eq!(poison.queue, "pipeline");
    assert_eq!(poison.reason, "unknown kind explode");
    assert!(poison.error.is_none(), "a quarantine has no handler error");
    assert_eq!(
        poison.envelope.payload,
        serde_json::json!({"image": "s3://detag-dev/poison.png"}),
        "the payload snapshot is the exact payload pushed"
    );
    let tampered_letter = letters
        .iter()
        .find(|l| l.job_id == tampered_id)
        .expect("tampered letter present");
    assert_eq!(tampered_letter.reason, "params_hash mismatch");
    assert_eq!(
        tampered_letter.envelope.payload,
        serde_json::json!({"image": "s3://detag-dev/swapped.png"}),
        "the snapshot keeps the tampered payload (the evidence)"
    );
    assert_eq!(
        dlq.count_dead("pipeline").await.expect("DLQ counts"),
        2,
        "two letters filed"
    );

    // 3. The worker survives the poison: a valid ping is processed
    // (dequeue count: the poison 1 + the ping 1; the tampered
    // envelope never reaches the dequeue — it fails the hash check).
    push_envelope(
        &config,
        JobEnvelope::new(Uuid::now_v7(), "ping", serde_json::json!({"probe": true})),
    )
    .await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if metrics
            .gather()
            .contains("detag_queue_dequeued_total{queue=\"pipeline\"} 2")
        {
            break;
        }
        if tokio::time::Instant::now() > deadline {
            panic!(
                "the ping was not processed after the poison:\n{}",
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
