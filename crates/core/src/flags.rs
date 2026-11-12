//! Feature flags: static (env) + DB-backed, cached (P0-021, P0-022).
//!
//! Two sources, one interface (`FlagResolver` — callers never ask where
//! a flag came from):
//!
//! - **Static (env):** `StaticFlagResolver` over P0-021's `FeatureFlags`
//!   (`PF_FEATURE_*` env vars). Defaults off; the deploy-time source.
//! - **DB-backed:** `CachedFlagResolver` over a `FlagStore` (the
//!   `feature_flags` table — key PK, enabled bool, rollout jsonb,
//!   updated_by, updated_at; see `migrations/0001_feature_flags.sql`
//!   and `docs/03-ARCHITECTURE/04-data-model.md`), cached with a 30 s
//!   TTL. An admin edit writes the row and invalidates the cache entry,
//!   so the flip is visible immediately; a change made outside the
//!   resolver (another process, direct SQL) is visible once the TTL
//!   expires.
//!
//! The store and the cache are both traits. What exists today:
//! `InMemoryFlagStore` + `InMemoryFlagCache` (tests and local dev).
//! What plugs in later, behind the same traits, when their clients
//! land: the sqlx `FlagStore` (E0.4) and the Redis `FlagCache` (E0.8).
//! The in-memory cache is **not** a production cache — in-proc caching
//! breaks NFR-10 (horizontal scaling); production uses Redis.
//!
//! Unknown flag keys resolve to **disabled** (fail-closed): a flag that
//! does not exist is off, never on. `rollout` is stored but not yet
//! evaluated by the resolver (percentage rollouts are future work).

use crate::config::{FeatureFlags, Settings};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

/// The DB-backed flag cache TTL (tech-stack doc): 30 s.
pub const FLAG_CACHE_TTL: Duration = Duration::from_secs(30);

/// One row of the `feature_flags` table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FlagRow {
    /// Flag key, e.g. `razorpay`, `provenance_strip`, `gpu`.
    pub key: String,
    /// Whether the flag is on.
    pub enabled: bool,
    /// Rollout config (e.g. percentage); stored, not yet evaluated.
    pub rollout: Option<JsonValue>,
    /// Who last changed it (user id, or `None` for system/seed).
    pub updated_by: Option<String>,
    /// Last change time.
    pub updated_at: DateTime<Utc>,
}

impl FlagRow {
    /// A new row, enabled as given, no rollout, stamped now.
    pub fn new(key: impl Into<String>, enabled: bool) -> Self {
        FlagRow {
            key: key.into(),
            enabled,
            rollout: None,
            updated_by: None,
            updated_at: Utc::now(),
        }
    }
}

/// The single interface callers use. Hides whether a flag comes from
/// env (static) or the DB (cached).
pub trait FlagResolver: Send + Sync {
    /// Whether `key` is on. Unknown keys are off (fail-closed).
    fn is_enabled(&self, key: &str) -> impl std::future::Future<Output = bool> + Send;
}

/// Static (env) resolver over P0-021's `FeatureFlags`.
pub struct StaticFlagResolver {
    flags: FeatureFlags,
}

impl StaticFlagResolver {
    /// Build from flags (e.g. `settings.feature_flags()`).
    pub fn new(flags: FeatureFlags) -> Self {
        StaticFlagResolver { flags }
    }

    /// Build from loaded settings.
    pub fn from_settings(settings: &Settings) -> Self {
        StaticFlagResolver::new(settings.feature_flags())
    }
}

impl FlagResolver for StaticFlagResolver {
    async fn is_enabled(&self, key: &str) -> bool {
        match key {
            "razorpay" => self.flags.razorpay,
            "provenance_strip" => self.flags.provenance_strip,
            "gpu" => self.flags.gpu,
            _ => false,
        }
    }
}

/// The repo interface: the `feature_flags` table. The sqlx
/// implementation lands with E0.4; the in-memory one is for tests.
pub trait FlagStore: Send + Sync {
    /// Read one row by key (`None` when the flag does not exist).
    fn get(&self, key: &str) -> impl std::future::Future<Output = Option<FlagRow>> + Send;
    /// Insert or replace one row (admin edit / seed).
    fn set(&self, row: FlagRow) -> impl std::future::Future<Output = ()> + Send;
}

/// In-memory `FlagStore`. Tests and local dev only.
pub struct InMemoryFlagStore {
    rows: Mutex<HashMap<String, FlagRow>>,
}

impl InMemoryFlagStore {
    /// Empty store.
    pub fn new() -> Self {
        InMemoryFlagStore {
            rows: Mutex::new(HashMap::new()),
        }
    }

    /// Store pre-filled with rows (test fixture).
    pub fn with_rows(rows: Vec<FlagRow>) -> Self {
        let store = Self::new();
        let mut map = store.rows.lock().unwrap();
        for row in rows {
            map.insert(row.key.clone(), row);
        }
        // Drop the guard before returning: it borrows `store.rows`,
        // and `store` cannot move out while borrowed.
        drop(map);
        store
    }
}

impl Default for InMemoryFlagStore {
    fn default() -> Self {
        Self::new()
    }
}

impl FlagStore for InMemoryFlagStore {
    async fn get(&self, key: &str) -> Option<FlagRow> {
        self.rows.lock().unwrap().get(key).cloned()
    }

    async fn set(&self, row: FlagRow) {
        self.rows.lock().unwrap().insert(row.key.clone(), row);
    }
}

/// `Arc` forwards to the inner store, so resolvers can be shared
/// (the api holds `Arc<CachedFlagResolver<...>>`).
impl<T: FlagStore> FlagStore for std::sync::Arc<T> {
    async fn get(&self, key: &str) -> Option<FlagRow> {
        self.as_ref().get(key).await
    }

    async fn set(&self, row: FlagRow) {
        self.as_ref().set(row).await;
    }
}

/// The cache interface: TTL entries + invalidation. The Redis
/// implementation plugs in behind this trait (E0.8).
pub trait FlagCache: Send + Sync {
    /// Cached value, if present and not expired.
    fn get(&self, key: &str) -> Option<bool>;
    /// Cache a value for `ttl`.
    fn set(&self, key: &str, enabled: bool, ttl: Duration);
    /// Drop one cached entry (admin edit path).
    fn invalidate(&self, key: &str);
}

/// In-memory TTL cache. Tests and local dev only — production uses
/// Redis (NFR-10).
pub struct InMemoryFlagCache {
    entries: Mutex<HashMap<String, (bool, Instant)>>,
}

impl InMemoryFlagCache {
    /// Empty cache.
    pub fn new() -> Self {
        InMemoryFlagCache {
            entries: Mutex::new(HashMap::new()),
        }
    }
}

impl Default for InMemoryFlagCache {
    fn default() -> Self {
        Self::new()
    }
}

impl FlagCache for InMemoryFlagCache {
    fn get(&self, key: &str) -> Option<bool> {
        let mut entries = self.entries.lock().unwrap();
        match entries.get(key) {
            Some((value, expires_at)) if *expires_at > Instant::now() => Some(*value),
            Some(_) => {
                entries.remove(key);
                None
            }
            None => None,
        }
    }

    fn set(&self, key: &str, enabled: bool, ttl: Duration) {
        self.entries
            .lock()
            .unwrap()
            .insert(key.to_string(), (enabled, Instant::now() + ttl));
    }

    fn invalidate(&self, key: &str) {
        self.entries.lock().unwrap().remove(key);
    }
}

/// `Arc` forwards to the inner cache, so resolvers can be shared.
impl<T: FlagCache> FlagCache for std::sync::Arc<T> {
    fn get(&self, key: &str) -> Option<bool> {
        self.as_ref().get(key)
    }

    fn set(&self, key: &str, enabled: bool, ttl: Duration) {
        self.as_ref().set(key, enabled, ttl);
    }

    fn invalidate(&self, key: &str) {
        self.as_ref().invalidate(key);
    }
}

/// DB-backed resolver: reads through the store, caches for `ttl`,
/// invalidates on edit. The 30 s default is `FLAG_CACHE_TTL`.
pub struct CachedFlagResolver<S, C> {
    store: S,
    cache: C,
    ttl: Duration,
}

impl<S: FlagStore, C: FlagCache> CachedFlagResolver<S, C> {
    /// Store + cache with the default 30 s TTL.
    pub fn new(store: S, cache: C) -> Self {
        Self::with_ttl(store, cache, FLAG_CACHE_TTL)
    }

    /// Store + cache with an explicit TTL (tests use a short one).
    pub fn with_ttl(store: S, cache: C, ttl: Duration) -> Self {
        CachedFlagResolver { store, cache, ttl }
    }

    /// Admin edit: write the row, then invalidate the cached entry so
    /// the flip is visible immediately (no waiting for the TTL).
    pub async fn set_flag(&self, row: FlagRow) {
        self.cache.invalidate(&row.key);
        self.store.set(row).await;
    }

    /// Drop one cached entry (admin edit path that writes the row
    /// elsewhere, e.g. direct SQL or another process's edit).
    pub fn invalidate(&self, key: &str) {
        self.cache.invalidate(key);
    }
}

impl<S: FlagStore, C: FlagCache> FlagResolver for CachedFlagResolver<S, C> {
    async fn is_enabled(&self, key: &str) -> bool {
        if let Some(cached) = self.cache.get(key) {
            return cached;
        }
        let enabled = self
            .store
            .get(key)
            .await
            .map(|row| row.enabled)
            .unwrap_or(false);
        self.cache.set(key, enabled, self.ttl);
        enabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// A store wrapper that counts `get` calls, proving the cache is hit.
    struct CountingStore {
        inner: InMemoryFlagStore,
        gets: AtomicUsize,
    }

    impl CountingStore {
        fn new(rows: Vec<FlagRow>) -> Self {
            CountingStore {
                inner: InMemoryFlagStore::with_rows(rows),
                gets: AtomicUsize::new(0),
            }
        }

        fn get_count(&self) -> usize {
            self.gets.load(Ordering::SeqCst)
        }
    }

    impl FlagStore for CountingStore {
        async fn get(&self, key: &str) -> Option<FlagRow> {
            self.gets.fetch_add(1, Ordering::SeqCst);
            self.inner.get(key).await
        }

        async fn set(&self, row: FlagRow) {
            self.inner.set(row).await;
        }
    }

    #[tokio::test]
    async fn static_resolver_reads_env_flags() {
        let flags = FeatureFlags {
            razorpay: true,
            provenance_strip: false,
            gpu: true,
        };
        let resolver = StaticFlagResolver::new(flags);
        assert!(resolver.is_enabled("razorpay").await);
        assert!(!resolver.is_enabled("provenance_strip").await);
        assert!(resolver.is_enabled("gpu").await);
        assert!(!resolver.is_enabled("no-such-flag").await);
    }

    #[tokio::test]
    async fn unknown_flag_is_disabled() {
        let store = InMemoryFlagStore::new();
        let resolver = CachedFlagResolver::new(store, InMemoryFlagCache::new());
        assert!(!resolver.is_enabled("no-such-flag").await);
    }

    #[tokio::test]
    async fn cache_avoids_store_reads_until_invalidated() {
        let store = CountingStore::new(vec![FlagRow::new("razorpay", true)]);
        let resolver = CachedFlagResolver::new(store, InMemoryFlagCache::new());
        assert!(resolver.is_enabled("razorpay").await);
        assert!(resolver.is_enabled("razorpay").await);
        assert_eq!(resolver.store.get_count(), 1, "second read must hit cache");
        resolver.invalidate("razorpay");
        assert!(resolver.is_enabled("razorpay").await);
        assert_eq!(
            resolver.store.get_count(),
            2,
            "read after invalidate hits store"
        );
    }

    /// AC: a flip is visible within the TTL — here the store is changed
    /// behind the resolver's back (another process / direct SQL), and the
    /// new value shows up once the TTL expires. Short TTL + generous
    /// sleep margin (100 ms TTL, 200 ms sleep) so the test is stable.
    #[tokio::test]
    async fn flip_visible_within_ttl() {
        let store = Arc::new(InMemoryFlagStore::with_rows(vec![FlagRow::new(
            "gpu", false,
        )]));
        let cache = Arc::new(InMemoryFlagCache::new());
        let resolver = CachedFlagResolver::with_ttl(
            Arc::clone(&store),
            Arc::clone(&cache),
            Duration::from_millis(100),
        );
        assert!(!resolver.is_enabled("gpu").await);
        // Flip behind the resolver's back (no invalidation).
        store.set(FlagRow::new("gpu", true)).await;
        // Still cached: the old value shows until the TTL expires.
        assert!(!resolver.is_enabled("gpu").await);
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(resolver.is_enabled("gpu").await, "flip visible after TTL");
    }

    /// AC: invalidation makes a flip visible immediately, no TTL wait.
    #[tokio::test]
    async fn invalidation_makes_flip_visible_immediately() {
        let store = InMemoryFlagStore::with_rows(vec![FlagRow::new("gpu", false)]);
        let resolver = CachedFlagResolver::new(store, InMemoryFlagCache::new());
        assert!(!resolver.is_enabled("gpu").await);
        // Admin edit: write + invalidate through the resolver.
        resolver.set_flag(FlagRow::new("gpu", true)).await;
        assert!(resolver.is_enabled("gpu").await, "flip visible at once");
    }

    #[tokio::test]
    async fn explicit_invalidate_after_external_edit() {
        let store = Arc::new(InMemoryFlagStore::with_rows(vec![FlagRow::new(
            "gpu", false,
        )]));
        let resolver = CachedFlagResolver::new(Arc::clone(&store), InMemoryFlagCache::new());
        assert!(!resolver.is_enabled("gpu").await);
        store.set(FlagRow::new("gpu", true)).await;
        assert!(!resolver.is_enabled("gpu").await, "still cached");
        resolver.invalidate("gpu");
        assert!(resolver.is_enabled("gpu").await, "visible after invalidate");
    }
}
