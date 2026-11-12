//! P0-079: the requeue-stuck sweep returns a dead worker's
//! jobs to the queue — and touches nothing else.
//!
//! Two envelopes are pushed to the pipeline queue, then both
//! are moved into in-flight sets the way `get_jobs.lua` moves
//! them when a worker fetches a job: one under a consumer
//! whose last keep-alive is 10 minutes stale (dead worker),
//! one under a consumer that heartbeated just now (live
//! worker). The sweep (`requeue_stuck`, the same call the
//! `just queue:requeue-stuck` command runs) must re-queue
//! exactly the dead worker's job — the live worker's job stays
//! in flight, a second sweep finds nothing, and a monitor
//! started afterwards processes the re-queued job (dequeued
//! 1), proving the job was not lost. The kill -9 drill itself
//! lands in Phase 3; this test proves the sweep half of NFR-21.

use apalis::prelude::*;
use apalis_redis::RedisStorage;
use detag_core::db::{connect_lazy, PoolConfig};
use detag_core::metrics::Metrics;
use detag_core::redis::{create_pool, RedisPoolConfig};
use detag_worker::{
    envelope::JobEnvelope,
    queue_hooks::QueueHooks,
    requeue::requeue_stuck,
    workers::{apalis_config, build_monitor, WorkerConfig},
};
use std::time::Duration;
use uuid::Uuid;

fn test_config() -> WorkerConfig {
    WorkerConfig {
        redis_url: std::env::var("PF_REDIS_URL")
            .unwrap_or_else(|_| "redis://localhost:6379".to_string()),
        concurrency: 1,
        namespace_prefix: format!("pf:test-requeue-{}", Uuid::now_v7()),
        dlq_prefix: format!("pf:test-dlq-{}", Uuid::now_v7()),
        visibility_timeout: Duration::from_secs(300),
        keep_alive: Duration::from_secs(30),
    }
}

/// Run a raw Redis command against the test Redis.
async fn redis_cmd<T: deadpool_redis::redis::FromRedisValue>(
    redis_url: &str,
    cmd: &str,
    args: &[&str],
) -> T {
    let pool = create_pool(redis_url, &RedisPoolConfig::default()).expect("pool builds");
    let mut conn = pool.get().await.expect("pool hands out a conn");
    let mut c = deadpool_redis::redis::cmd(cmd);
    for arg in args {
        c.arg(*arg);
    }
    c.query_async(&mut conn).await.expect("command runs")
}

#[tokio::test]
async fn sweep_requeues_only_the_dead_workers_jobs() {
    let config = test_config();
    let namespace = config.queue_namespace("pipeline");
    let active = format!("{namespace}:active");
    let consumers = format!("{namespace}:consumers");
    let dead_inflight = format!("{namespace}:inflight:dead-worker");
    let live_inflight = format!("{namespace}:inflight:live-worker");
    let now = chrono::Utc::now().timestamp();

    // Push two envelopes; `push` returns the apalis task id.
    let conn = apalis_redis::connect(config.redis_url.as_str())
        .await
        .expect("Redis connects");
    let mut storage = RedisStorage::new_with_config(conn, apalis_config(&config, "pipeline"));
    let stuck = storage
        .push(JobEnvelope::new(
            Uuid::now_v7(),
            "ping",
            serde_json::json!({"probe": "stuck"}),
        ))
        .await
        .expect("stuck envelope enqueues");
    let live = storage
        .push(JobEnvelope::new(
            Uuid::now_v7(),
            "ping",
            serde_json::json!({"probe": "live"}),
        ))
        .await
        .expect("live envelope enqueues");
    let stuck_id = stuck.task_id.to_string();
    let live_id = live.task_id.to_string();

    // Simulate two workers fetching: the job leaves the active
    // list and joins its worker's in-flight set, and the worker
    // registers (or re-registers) its consumer with its last
    // keep-alive timestamp.
    for (job_id, inflight, consumer_score) in [
        (stuck_id.as_str(), dead_inflight.as_str(), now - 600),
        (live_id.as_str(), live_inflight.as_str(), now),
    ] {
        let _: i64 = redis_cmd(&config.redis_url, "LREM", &[&active, "1", job_id]).await;
        let _: i64 = redis_cmd(&config.redis_url, "SADD", &[inflight, job_id]).await;
        let _: i64 = redis_cmd(
            &config.redis_url,
            "ZADD",
            &[&consumers, &consumer_score.to_string(), inflight],
        )
        .await;
    }
    let active_len: i64 = redis_cmd(&config.redis_url, "LLEN", &[&active]).await;
    assert_eq!(active_len, 0, "both jobs are in flight before the sweep");

    // The sweep: older-than 300 s — only the dead worker qualifies.
    let swept = requeue_stuck(
        &config.redis_url,
        &["pipeline"],
        &config.namespace_prefix,
        Duration::from_secs(300),
        100,
    )
    .await
    .expect("sweep runs");
    assert_eq!(
        swept,
        vec![("pipeline".to_string(), 1)],
        "exactly one orphan"
    );

    let active_len: i64 = redis_cmd(&config.redis_url, "LLEN", &[&active]).await;
    assert_eq!(
        active_len, 1,
        "the dead worker's job is back on the active list"
    );
    let dead_left: i64 = redis_cmd(&config.redis_url, "SCARD", &[&dead_inflight]).await;
    assert_eq!(dead_left, 0, "the dead worker's in-flight set is drained");
    let live_left: i64 = redis_cmd(&config.redis_url, "SCARD", &[&live_inflight]).await;
    assert_eq!(live_left, 1, "the live worker's job was not stolen");
    let consumers_left: i64 = redis_cmd(&config.redis_url, "ZCARD", &[&consumers]).await;
    assert_eq!(
        consumers_left, 1,
        "the dead consumer is unregistered; the live one stays"
    );

    // A second sweep finds nothing — the sweep is idempotent.
    let swept_again = requeue_stuck(
        &config.redis_url,
        &["pipeline"],
        &config.namespace_prefix,
        Duration::from_secs(300),
        100,
    )
    .await
    .expect("second sweep runs");
    assert_eq!(swept_again, vec![("pipeline".to_string(), 0)]);

    // The re-queued job is not lost: a monitor started now
    // processes it (dequeued 1 — the live worker's job stays
    // in flight, so it is never counted).
    let metrics = Metrics::new();
    let hooks = QueueHooks::new(metrics.queue().clone());
    hooks.reset_depths();
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
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if metrics
            .gather()
            .contains("detag_queue_dequeued_total{queue=\"pipeline\"} 1")
        {
            break;
        }
        if tokio::time::Instant::now() > deadline {
            panic!("the re-queued job was not processed:\n{}", metrics.gather());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    shutdown_tx.send(()).expect("monitor still running");
    let exited = tokio::time::timeout(Duration::from_secs(10), monitor_task)
        .await
        .expect("monitor shuts down within 10 s");
    exited
        .expect("monitor task joins")
        .expect("monitor exits cleanly");
}
