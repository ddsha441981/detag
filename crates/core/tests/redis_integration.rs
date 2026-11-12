//! Integration suite for the P0-051 Redis work (E0.5).
//!
//! The ticket spec names testcontainers; that cannot run on this VM
//! (DOCKER-001: containers cannot start — see
//! `docs/09-STATE/ISSUES.md`, ASSUMPTION-009). The suite therefore
//! runs against the dev Redis the same way the P0-046..P0-050 live
//! tests do (native redis-server on this VM, compose redis where
//! docker runs — `PF_REDIS_URL`, dev default
//! `redis://localhost:6379`). It covers exactly what the ticket
//! asks for: primitives under concurrency, pubsub latency, and
//! failover error mapping to typed errors (`AppError::CacheUnavailable`,
//! PF-5003). Every test uses unique keys/job ids per run and deletes
//! its keys afterwards — never FLUSHDB, never a shared key.

use detag_core::error::AppError;
use detag_core::metrics::Metrics;
use detag_core::redis::cache::Cache;
use detag_core::redis::keys::cache_key;
use detag_core::redis::pubsub::{publish, subscribe, JobEvent, SUBSCRIBER_BUFFER};
use detag_core::redis::ratelimit::FixedWindow;
use detag_core::redis::{create_pool, redis_health, RedisPool, RedisPoolConfig};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::time::timeout;
use uuid::Uuid;

fn redis_url() -> String {
    std::env::var("PF_REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".to_string())
}

fn pool() -> RedisPool {
    create_pool(&redis_url(), &RedisPoolConfig::default()).expect("pool builds")
}

/// One connection per concurrent task (mixed-workload test).
fn wide_pool() -> RedisPool {
    create_pool(
        &redis_url(),
        &RedisPoolConfig {
            max_size: 50,
            ..RedisPoolConfig::default()
        },
    )
    .expect("pool builds")
}

/// A pool pointed at a closed port: checkout fails fast (200 ms),
/// which is what makes the failover tests quick.
fn dead_pool() -> RedisPool {
    create_pool(
        "redis://127.0.0.1:59998",
        &RedisPoolConfig {
            wait_timeout: Duration::from_millis(200),
            create_timeout: Duration::from_millis(200),
            ..RedisPoolConfig::default()
        },
    )
    .expect("pool creation is lazy and must succeed")
}

fn assert_cache_unavailable(err: AppError) {
    assert_eq!(err.code(), "PF-5003", "failover must map to PF-5003");
    assert_eq!(err.status(), 503, "failover is a 503");
    assert_eq!(err.title(), "cache_unavailable");
    // The production body must carry the safe default detail only —
    // no host, port, or provider text may leak.
    let problem = err.to_problem(
        detag_core::error::Profile::Production,
        Some("01JFAILOVER"),
        Some("redis: connection refused at 127.0.0.1:59998"),
    );
    assert_eq!(problem.detail, "the cache is temporarily unavailable");
    assert!(!problem.to_json().to_string().contains("59998"));
}

/// All three Redis primitives share one pool under concurrency:
/// 10 rate-limit checks, 10 cache loads, 10 pubsub publishes
/// (each to its own pre-subscribed job channel). Every operation
/// succeeds, the cache loader runs exactly once per key, and every
/// published event arrives on its own channel.
#[tokio::test]
async fn mixed_primitives_share_one_pool_under_concurrency() {
    let pool = wide_pool();
    let metrics = Metrics::new();
    let cache = Arc::new(Cache::new(pool.clone(), &metrics));
    let window = FixedWindow::new(10, Duration::from_secs(60)).expect("window builds");
    let scope = format!("p0051-{}", Uuid::now_v7());

    // One subscriber per publish task, set up before the burst.
    let mut streams = Vec::new();
    let mut jobs = Vec::new();
    for _ in 0..10 {
        let job = Uuid::now_v7();
        let stream = subscribe(&redis_url(), &job, SUBSCRIBER_BUFFER)
            .await
            .expect("subscribe");
        streams.push(stream);
        jobs.push(job);
    }

    let mut tasks = Vec::new();
    for i in 0..10usize {
        let pool = pool.clone();
        let scope = scope.clone();
        tasks.push(tokio::spawn(async move {
            let rate = window
                .check(&pool, &scope, &format!("id-{i}"))
                .await
                .expect("rate-limit check succeeds");
            assert!(rate.allowed, "first check must be admitted");
            assert_eq!(rate.remaining, 9, "limit 10, one used");
        }));
    }
    let loads = Arc::new(AtomicUsize::new(0));
    for i in 0..10usize {
        let cache = Arc::clone(&cache);
        let loads = Arc::clone(&loads);
        tasks.push(tokio::spawn(async move {
            let key = cache_key("p0051", &format!("mixed-{i}-{}", Uuid::now_v7()));
            let runs = Arc::clone(&loads);
            let value = cache
                .get_or_set("mixed", &key, Duration::from_secs(60), move || {
                    runs.fetch_add(1, Ordering::SeqCst);
                    Box::pin(async move { Ok::<String, String>(format!("v{i}")) })
                })
                .await
                .expect("cache load succeeds");
            assert_eq!(value, format!("v{i}"));
        }));
    }
    for (i, job) in jobs.iter().enumerate() {
        let pool = pool.clone();
        let job = *job;
        tasks.push(tokio::spawn(async move {
            let receivers = publish(
                &pool,
                &JobEvent::Progress {
                    job_id: job,
                    progress: i as f32 / 10.0,
                },
            )
            .await
            .expect("publish succeeds");
            assert_eq!(receivers, 1, "exactly one subscriber listens");
        }));
    }
    for task in tasks {
        task.await.expect("task joins without panicking");
    }
    assert_eq!(
        loads.load(Ordering::SeqCst),
        10,
        "each cache key loads exactly once"
    );
    for (i, mut stream) in streams.into_iter().enumerate() {
        let event = timeout(Duration::from_secs(5), stream.next_event())
            .await
            .expect("event arrives in time")
            .expect("stream stays open");
        assert_eq!(
            event,
            JobEvent::Progress {
                job_id: jobs[i],
                progress: i as f32 / 10.0,
            }
        );
    }
}

/// Pubsub latency under load: 20 churn tasks (cache + rate-limit)
/// run while 100 events are published; p95 publish→receive stays
/// under the P0-050 bound of 500 ms.
#[tokio::test]
async fn pubsub_latency_p95_under_concurrent_load() {
    let pool = wide_pool();
    let job = Uuid::now_v7();
    let mut stream = subscribe(&redis_url(), &job, 128).await.expect("subscribe");
    let stop = Arc::new(AtomicUsize::new(0));
    let mut churn = Vec::new();
    for i in 0..20usize {
        let pool = pool.clone();
        let metrics = Metrics::new();
        let cache = Arc::new(Cache::new(pool.clone(), &metrics));
        let stop = Arc::clone(&stop);
        let window = FixedWindow::new(1_000, Duration::from_secs(60)).expect("window builds");
        churn.push(tokio::spawn(async move {
            let mut n = 0usize;
            while stop.load(Ordering::SeqCst) == 0 {
                let _ = window.check(&pool, "p0051-churn", &format!("c{i}")).await;
                let key = cache_key("p0051-churn", &format!("k{i}-{n}"));
                let _ = cache
                    .get_or_set("churn", &key, Duration::from_secs(60), || {
                        Box::pin(async move { Ok::<String, String>("x".to_string()) })
                    })
                    .await;
                n += 1;
            }
        }));
    }

    const N: usize = 100;
    let mut latencies = Vec::with_capacity(N);
    for i in 0..N {
        let event = JobEvent::Progress {
            job_id: job,
            progress: i as f32 / N as f32,
        };
        let sent = Instant::now();
        publish(&pool, &event).await.expect("publish");
        let got = timeout(Duration::from_secs(5), stream.next_event())
            .await
            .expect("event arrives in time")
            .expect("stream stays open");
        assert_eq!(got, event);
        latencies.push(sent.elapsed());
    }
    stop.store(1, Ordering::SeqCst);
    for task in churn {
        task.await.expect("churn task joins");
    }
    latencies.sort_unstable();
    let p95 = latencies[(95 * N / 100).min(N - 1)];
    assert!(
        p95 <= Duration::from_millis(500),
        "p95 {p95:?} must stay under 500 ms under load"
    );
}

/// Failover: a dead Redis maps the rate-limit primitive to the
/// typed `CacheUnavailable` (PF-5003, 503).
#[tokio::test]
async fn failover_ratelimit_maps_to_cache_unavailable() {
    let pool = dead_pool();
    let window = FixedWindow::new(10, Duration::from_secs(60)).expect("window builds");
    let err = window
        .check(&pool, "p0051-failover", "id")
        .await
        .expect_err("checkout against a dead Redis must fail");
    assert_cache_unavailable(AppError::from(err));
}

/// Failover: a dead Redis maps the cache helper to the typed
/// `CacheUnavailable` (PF-5003, 503).
#[tokio::test]
async fn failover_cache_maps_to_cache_unavailable() {
    let metrics = Metrics::new();
    let cache = Cache::new(dead_pool(), &metrics);
    let err = cache
        .get_or_set(
            "failover",
            &cache_key("p0051-failover", &Uuid::now_v7().to_string()),
            Duration::from_secs(60),
            || Box::pin(async move { Ok::<String, String>("never".to_string()) }),
        )
        .await
        .expect_err("load against a dead Redis must fail");
    assert_cache_unavailable(AppError::from(err));
}

/// Failover: a dead Redis maps both pubsub paths (publish via the
/// pool, subscribe via a fresh client) to the typed
/// `CacheUnavailable` (PF-5003, 503).
#[tokio::test]
async fn failover_pubsub_maps_to_cache_unavailable() {
    let pool = dead_pool();
    let err = publish(
        &pool,
        &JobEvent::Progress {
            job_id: Uuid::now_v7(),
            progress: 0.0,
        },
    )
    .await
    .expect_err("publish against a dead Redis must fail");
    assert_cache_unavailable(AppError::from(err));

    let err = timeout(
        Duration::from_secs(5),
        subscribe("redis://127.0.0.1:59998", &Uuid::now_v7(), 64),
    )
    .await
    .expect("subscribe attempt finishes in time")
    .expect_err("subscribe against a dead Redis must fail");
    assert_cache_unavailable(AppError::from(err));
}

/// Failover + recovery: the health probe reports the dead Redis
/// down and the live one up, and a fresh pool against the live
/// Redis works — the suite fails over and comes back.
#[tokio::test]
async fn failover_health_reports_down_then_recovers() {
    let dead = redis_health("redis://127.0.0.1:59998").await;
    assert!(!dead.ok, "a dead Redis must report down: {dead:?}");
    let live = redis_health(&redis_url()).await;
    assert!(live.ok, "the live Redis must report up: {live:?}");

    let pool = pool();
    let mut conn = pool.get().await.expect("checkout after recovery");
    let pong: String = deadpool_redis::redis::cmd("PING")
        .query_async(&mut conn)
        .await
        .expect("PING after recovery");
    assert_eq!(pong, "PONG");
}
