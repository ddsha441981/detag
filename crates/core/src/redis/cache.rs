//! Cache helper (P0-049): `get_or_set` with TTL + singleflight +
//! invalidation, over the Redis pool.
//!
//! Values are JSON (serde_json) under keys built with the
//! [`crate::redis::keys`] helpers (`pf:cache:{kind}:{id}` for
//! generic entries). `kind` is the metric label — a
//! low-cardinality caller name (e.g. `plan`, `flags`), never a raw
//! id.
//!
//! Singleflight is local (in-process): concurrent `get_or_set`
//! calls for the same cold key share one loader run instead of
//! stampeding Redis and the loader. A loader failure is NOT
//! cached — the next caller retries the load. A corrupt cached
//! value (not JSON) is treated as a miss and overwritten.

use crate::metrics::{CacheMetrics, Metrics};
use crate::redis::RedisPool;
use deadpool_redis::redis::{cmd, AsyncCommands};
use serde::{de::DeserializeOwned, Serialize};
use std::collections::HashMap;
use std::fmt::Display;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::sync::Mutex;

/// Cache failures. Loader errors are rendered to a string
/// (`Load`) — the loader's error type only needs `Display`, and
/// the cache never needs to match on it.
#[derive(Debug, Error)]
pub enum CacheError {
    /// Bad call (e.g. a TTL under 1 s).
    #[error("invalid cache call: {0}")]
    Invalid(String),
    /// Checkout from the pool failed.
    #[error("redis pool error: {0}")]
    Pool(#[from] deadpool_redis::PoolError),
    /// A Redis command failed.
    #[error("redis error: {0}")]
    Redis(#[from] deadpool_redis::redis::RedisError),
    /// The value could not be serialized to JSON.
    #[error("value is not serializable: {0}")]
    Encode(String),
    /// The loader failed (not cached — the next caller retries).
    #[error("loader failed: {0}")]
    Load(String),
}

/// In-process singleflight: at most one in-flight load per key.
/// A key's mutex is dropped from the map when its load finishes;
/// a task arriving in the gap between the drop and the removal
/// may start a second load — benign (both loads write the same
/// value, and the cache is correct either way). Failures are not
/// deduplicated: a failed load releases the mutex and the next
/// waiter becomes the loader.
#[derive(Debug, Default)]
struct Singleflight {
    map: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

impl Singleflight {
    /// Run `fut` holding the mutex for `key` (created on demand).
    async fn run<F, Fut, T>(&self, key: &str, fut: F) -> T
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = T>,
    {
        let mutex = {
            let mut map = self.map.lock().await;
            map.entry(key.to_string())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let guard = mutex.clone().lock_owned().await;
        let result = fut().await;
        drop(guard);
        // Remove the entry if it is still ours (a newer load for
        // the same key may have replaced it — never remove that).
        let mut map = self.map.lock().await;
        if let Some(current) = map.get(key) {
            if Arc::ptr_eq(current, &mutex) {
                map.remove(key);
            }
        }
        result
    }
}

/// The cache: a pool, a singleflight group, and hit/miss metrics.
#[derive(Debug)]
pub struct Cache {
    pool: RedisPool,
    singleflight: Singleflight,
    metrics: CacheMetrics,
}

impl Cache {
    /// Build a cache over `pool`, reporting hits/misses into
    /// `metrics` (one `Metrics` per process — pass the process's).
    pub fn new(pool: RedisPool, metrics: &Metrics) -> Self {
        Cache {
            pool,
            singleflight: Singleflight::default(),
            metrics: metrics.cache().clone(),
        }
    }

    /// The cached value for `key`, if present and valid JSON.
    /// Corrupt entries count as absent (the caller reloads and
    /// overwrites them).
    async fn get<T>(&self, key: &str) -> Result<Option<T>, CacheError>
    where
        T: DeserializeOwned,
    {
        let mut conn = self.pool.get().await?;
        let raw: Option<String> = conn.get(key).await?;
        match raw {
            None => Ok(None),
            Some(json) => match serde_json::from_str(&json) {
                Ok(value) => Ok(Some(value)),
                Err(_) => Ok(None),
            },
        }
    }

    /// Store `value` under `key` for `ttl` (SETEX).
    async fn set_raw(&self, key: &str, json: &str, ttl: Duration) -> Result<(), CacheError> {
        let mut conn = self.pool.get().await?;
        cmd("SETEX")
            .arg(key)
            .arg(ttl.as_secs() as i64)
            .arg(json)
            .query_async::<()>(&mut conn)
            .await?;
        Ok(())
    }

    /// Return the cached value for `key`, or load it (once, even
    /// under concurrency), store it for `ttl`, and return it.
    /// `kind` is the metric label. `ttl` must be >= 1 s.
    pub async fn get_or_set<T, E, F, Fut>(
        &self,
        kind: &str,
        key: &str,
        ttl: Duration,
        loader: F,
    ) -> Result<T, CacheError>
    where
        T: Serialize + DeserializeOwned,
        E: Display,
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, E>>,
    {
        if ttl.as_secs() < 1 {
            return Err(CacheError::Invalid("ttl must be >= 1 s".to_string()));
        }
        // Fast path: no lock, no loader.
        if let Some(value) = self.get::<T>(key).await? {
            self.metrics.hit(kind);
            return Ok(value);
        }
        // Singleflight: one load per key. The re-check inside is
        // the second chance for a waiter to hit instead of loading.
        self.singleflight
            .run(key, async || {
                if let Some(value) = self.get::<T>(key).await? {
                    self.metrics.hit(kind);
                    return Ok(value);
                }
                self.metrics.miss(kind);
                let value = loader()
                    .await
                    .map_err(|e| CacheError::Load(e.to_string()))?;
                let json =
                    serde_json::to_string(&value).map_err(|e| CacheError::Encode(e.to_string()))?;
                self.set_raw(key, &json, ttl).await?;
                Ok(value)
            })
            .await
    }

    /// Drop one entry (`DEL`). The next `get_or_set` reloads it.
    pub async fn invalidate(&self, key: &str) -> Result<(), CacheError> {
        let mut conn = self.pool.get().await?;
        let _: i32 = cmd("DEL").arg(key).query_async(&mut conn).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn singleflight_serializes_loads_for_one_key() {
        // The primitive serializes: loaders for one key never
        // overlap (max concurrency 1). It does NOT dedup by
        // itself — dedup is the caller's re-check inside the lock
        // (Cache::get_or_set), proven live by the stampede test.
        let group = Arc::new(Singleflight::default());
        let runs = Arc::new(AtomicUsize::new(0));
        let live = Arc::new(AtomicUsize::new(0));
        let max_live = Arc::new(AtomicUsize::new(0));
        let mut tasks = Vec::new();
        for _ in 0..50 {
            let group = Arc::clone(&group);
            let runs = Arc::clone(&runs);
            let live = Arc::clone(&live);
            let max_live = Arc::clone(&max_live);
            tasks.push(tokio::spawn(async move {
                group
                    .run("k", async || {
                        runs.fetch_add(1, Ordering::SeqCst);
                        let now = live.fetch_add(1, Ordering::SeqCst) + 1;
                        max_live.fetch_max(now, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(10)).await;
                        live.fetch_sub(1, Ordering::SeqCst);
                        42u32
                    })
                    .await
            }));
        }
        let mut results = Vec::new();
        for task in tasks {
            results.push(task.await.expect("task joins"));
        }
        assert!(results.iter().all(|&v| v == 42));
        assert_eq!(
            runs.load(Ordering::SeqCst),
            50,
            "the primitive serializes, it does not dedup — all 50 loads run"
        );
        assert_eq!(
            max_live.load(Ordering::SeqCst),
            1,
            "no two loads for one key may overlap"
        );
    }

    #[tokio::test]
    async fn singleflight_does_not_serialize_different_keys() {
        // B's loader must START while A's loader is still running —
        // a single global lock would block B until A finishes. No
        // timing bound: A sleeps, then checks the flag B sets when
        // its loader starts.
        let group = Arc::new(Singleflight::default());
        let b_started = Arc::new(AtomicUsize::new(0));
        let b_started_clone = Arc::clone(&b_started);
        let group_a = Arc::clone(&group);
        let a = tokio::spawn(async move {
            group_a
                .run("a", async || {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    assert_eq!(
                        b_started_clone.load(Ordering::SeqCst),
                        1,
                        "key b's loader must start while key a's loader runs"
                    );
                    "a"
                })
                .await
        });
        let b_started2 = Arc::clone(&b_started);
        let group_b = Arc::clone(&group);
        let b = tokio::spawn(async move {
            group_b
                .run("b", async || {
                    b_started2.fetch_add(1, Ordering::SeqCst);
                    "b"
                })
                .await
        });
        assert_eq!(
            (a.await.expect("task joins"), b.await.expect("task joins")),
            ("a", "b")
        );
    }
}
