//! Live tests for the P0-049 cache helper (`get_or_set`).
//!
//! Against the dev Redis (native redis-server on this VM, compose
//! redis where docker runs — PF_REDIS_URL, dev default
//! `redis://localhost:6379`). Every test uses a unique key
//! (`pf:cache:test:{uuid}`) so parallel runs never share an entry.

use detag_core::metrics::Metrics;
use detag_core::redis::cache::Cache;
use detag_core::redis::keys::cache_key;
use detag_core::redis::{create_pool, RedisPool, RedisPoolConfig};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

fn redis_url() -> String {
    std::env::var("PF_REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".to_string())
}

fn pool() -> RedisPool {
    create_pool(&redis_url(), &RedisPoolConfig::default()).expect("pool builds")
}

/// A pool with one connection per concurrent task (stampede test).
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

fn test_key() -> String {
    cache_key("test", &Uuid::now_v7().to_string())
}

/// A loader that counts its runs and returns `v{n}` per run.
fn counting_loader(
    runs: Arc<AtomicUsize>,
) -> impl FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
{
    move || {
        let n = runs.fetch_add(1, Ordering::SeqCst) + 1;
        Box::pin(async move { Ok(format!("v{n}")) })
    }
}

#[tokio::test]
async fn miss_then_hit_and_metrics_emitted() {
    let metrics = Metrics::new();
    let cache = Cache::new(pool(), &metrics);
    let key = test_key();
    let runs = Arc::new(AtomicUsize::new(0));

    let first = cache
        .get_or_set(
            "test",
            &key,
            Duration::from_secs(60),
            counting_loader(Arc::clone(&runs)),
        )
        .await
        .expect("first call loads");
    assert_eq!(first, "v1");
    assert_eq!(runs.load(Ordering::SeqCst), 1);

    let second = cache
        .get_or_set(
            "test",
            &key,
            Duration::from_secs(60),
            counting_loader(Arc::clone(&runs)),
        )
        .await
        .expect("second call hits");
    assert_eq!(second, "v1", "the cached value must be returned");
    assert_eq!(
        runs.load(Ordering::SeqCst),
        1,
        "the loader must not run again"
    );

    let gathered = metrics.gather();
    assert!(
        gathered.contains("detag_cache_misses_total{kind=\"test\"} 1"),
        "one miss must be recorded:\n{gathered}"
    );
    assert!(
        gathered.contains("detag_cache_hits_total{kind=\"test\"} 1"),
        "one hit must be recorded:\n{gathered}"
    );
}

#[tokio::test]
async fn ttl_expiry_reloads() {
    let metrics = Metrics::new();
    let cache = Cache::new(pool(), &metrics);
    let key = test_key();
    let runs = Arc::new(AtomicUsize::new(0));

    let first = cache
        .get_or_set(
            "test",
            &key,
            Duration::from_secs(1),
            counting_loader(Arc::clone(&runs)),
        )
        .await
        .expect("first call loads");
    assert_eq!(first, "v1");
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let second = cache
        .get_or_set(
            "test",
            &key,
            Duration::from_secs(1),
            counting_loader(Arc::clone(&runs)),
        )
        .await
        .expect("call after expiry reloads");
    assert_eq!(second, "v2", "an expired entry must be reloaded");
    assert_eq!(runs.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn invalidate_reloads() {
    let metrics = Metrics::new();
    let cache = Cache::new(pool(), &metrics);
    let key = test_key();
    let runs = Arc::new(AtomicUsize::new(0));

    let first = cache
        .get_or_set(
            "test",
            &key,
            Duration::from_secs(60),
            counting_loader(Arc::clone(&runs)),
        )
        .await
        .expect("first call loads");
    assert_eq!(first, "v1");
    cache.invalidate(&key).await.expect("invalidate");
    let second = cache
        .get_or_set(
            "test",
            &key,
            Duration::from_secs(60),
            counting_loader(Arc::clone(&runs)),
        )
        .await
        .expect("call after invalidate reloads");
    assert_eq!(second, "v2");
    assert_eq!(runs.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn corrupt_entry_is_treated_as_miss() {
    let metrics = Metrics::new();
    let cache = Cache::new(pool(), &metrics);
    let key = test_key();
    let runs = Arc::new(AtomicUsize::new(0));

    // Plant a non-JSON value under the key with a raw command.
    let pool = pool();
    let mut conn = pool.get().await.expect("checkout");
    let _: () = deadpool_redis::redis::cmd("SET")
        .arg(&key)
        .arg("this is not json")
        .query_async(&mut conn)
        .await
        .expect("plant corrupt entry");

    let value = cache
        .get_or_set(
            "test",
            &key,
            Duration::from_secs(60),
            counting_loader(Arc::clone(&runs)),
        )
        .await
        .expect("corrupt entry must reload");
    assert_eq!(value, "v1");
    assert_eq!(runs.load(Ordering::SeqCst), 1);

    // The corrupt entry was overwritten with valid JSON.
    let raw: Option<String> = deadpool_redis::redis::cmd("GET")
        .arg(&key)
        .query_async(&mut conn)
        .await
        .expect("read back");
    assert_eq!(raw.as_deref(), Some("\"v1\""));
}

#[tokio::test]
async fn stampede_loads_once() {
    let metrics = Metrics::new();
    let cache = Arc::new(Cache::new(wide_pool(), &metrics));
    let key = test_key();
    let runs = Arc::new(AtomicUsize::new(0));

    let mut tasks = Vec::new();
    for _ in 0..50 {
        let cache = Arc::clone(&cache);
        let key = key.clone();
        let runs = Arc::clone(&runs);
        tasks.push(tokio::spawn(async move {
            cache
                .get_or_set("test", &key, Duration::from_secs(60), move || {
                    let n = runs.fetch_add(1, Ordering::SeqCst) + 1;
                    async move {
                        // Slow loader: without singleflight, all 50
                        // tasks would run it.
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        Ok::<String, String>(format!("v{n}"))
                    }
                })
                .await
        }));
    }
    let mut values = Vec::new();
    for task in tasks {
        values.push(
            task.await
                .expect("task joins")
                .expect("get_or_set succeeds"),
        );
    }
    assert!(
        values.iter().all(|v| v == "v1"),
        "all 50 callers must get the one loaded value: {values:?}"
    );
    assert_eq!(
        runs.load(Ordering::SeqCst),
        1,
        "50 concurrent calls on a cold key must run the loader once"
    );
    let gathered = metrics.gather();
    assert!(
        gathered.contains("detag_cache_misses_total{kind=\"test\"} 1"),
        "exactly one miss must be recorded:\n{gathered}"
    );
}

#[tokio::test]
async fn loader_failure_is_not_cached() {
    let metrics = Metrics::new();
    let cache = Cache::new(pool(), &metrics);
    let key = test_key();
    let runs = Arc::new(AtomicUsize::new(0));

    let runs1 = Arc::clone(&runs);
    let err = cache
        .get_or_set("test", &key, Duration::from_secs(60), move || {
            runs1.fetch_add(1, Ordering::SeqCst);
            async move { Err::<String, String>("boom".to_string()) }
        })
        .await
        .expect_err("a failing loader must propagate the error");
    assert!(
        err.to_string().contains("boom"),
        "error must surface: {err}"
    );
    assert_eq!(runs.load(Ordering::SeqCst), 1);

    let second = cache
        .get_or_set(
            "test",
            &key,
            Duration::from_secs(60),
            counting_loader(Arc::clone(&runs)),
        )
        .await
        .expect("retry after a failure must load");
    assert_eq!(second, "v2", "the retry is the second loader run");
    assert_eq!(runs.load(Ordering::SeqCst), 2);
}
