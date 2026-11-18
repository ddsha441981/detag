//! Plans catalog + entitlements (P1-049, FR-P-01).
//!
//! The `plans` table is the catalog: seeded by migration 003
//! (Free / Pro / Business, amounts in paise, entitlements
//! jsonb) and owner-changeable per FR-P-01 — admin edits go
//! through the admin API (P1-056), never through this module.
//! This module is the single place that interprets the
//! entitlements jsonb and the single place that serves the
//! catalog:
//!
//! - [`Entitlements`]: the parsed entitlement struct — the
//!   seven keys the data model names (`daily_images`,
//!   `daily_video_seconds`, `max_bytes`, `result_ttl_hours`,
//!   `concurrency`, `lane`, `features`). Parsing is
//!   fail-closed: a missing, mistyped, or negative numeric
//!   key reads as 0 (a plan that does not grant a quota
//!   grants nothing), an unknown lane reads as `standard`.
//! - [`PlanCatalog`]: the catalog from the DB, cached for
//!   [`PLAN_CACHE_TTL`]. It follows the `flags.rs` shape
//!   exactly — a [`PlanStore`] trait (the DB) + a
//!   [`PlanCache`] trait (the TTL cache) + a cached reader
//!   over both, with explicit [`PlanCatalog::invalidate`] for
//!   the admin edit path. What exists today:
//!   [`SqlxPlanStore`] + [`InMemoryPlanCache`] (tests and
//!   local dev). What plugs in later, behind the same
//!   traits, when its client lands: the Redis `PlanCache`
//!   (the in-memory cache is **not** a production cache —
//!   in-proc caching breaks NFR-10 horizontal scaling;
//!   production uses Redis).
//!
//! Effective-plan resolution — which plan a user is on —
//! lives in the API service
//! (`apps/api/src/services/entitlements.rs::effective_plan`):
//! the newest live subscription (`trialing`/`active`)
//! overrides the Free default. The service reads its plans
//! through this catalog, so a plan edit is visible to
//! resolution as soon as the catalog invalidates or the TTL
//! expires.

use crate::db::DbPool;
use crate::repos::{plans::Plan, RepoError};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};
use uuid::Uuid;

/// The plan-catalog cache TTL (P1-049, ADR-0025): 5 minutes.
/// Plans change rarely (owner/admin edits only) and every
/// edit path invalidates explicitly, so a stale catalog can
/// live at most one TTL after an edit made outside the
/// resolver (another process, direct SQL).
pub const PLAN_CACHE_TTL: Duration = Duration::from_secs(300);

/// Which lane a plan's jobs run in. Unknown values read as
/// `standard` (fail closed — the priority lane is the
/// higher-privilege one).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Lane {
    #[default]
    Standard,
    Priority,
}

/// The seven entitlements of a plan, parsed from the
/// `plans.entitlements` jsonb. All numbers are `i64`:
/// `max_bytes` for Business is 4 GiB, past `i32::MAX`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Entitlements {
    /// Images per UTC day (0 = no images — fail closed).
    pub daily_images: i64,
    /// Video seconds per UTC day (0 = no video).
    pub daily_video_seconds: i64,
    /// Largest single upload, in bytes.
    pub max_bytes: i64,
    /// How long a finished result is kept, in hours.
    pub result_ttl_hours: i64,
    /// Concurrent running jobs for the user.
    pub concurrency: i64,
    /// Which lane the plan's jobs run in.
    pub lane: Lane,
    /// Named feature flags (`api_keys`, `webhooks`, `batch`).
    /// `priority_lane` is NOT a map entry — it reads the
    /// `lane` field (see [`Entitlements::grants`]).
    pub features: HashMap<String, bool>,
}

impl Default for Entitlements {
    /// The fail-closed entitlements: nothing granted.
    fn default() -> Self {
        Entitlements {
            daily_images: 0,
            daily_video_seconds: 0,
            max_bytes: 0,
            result_ttl_hours: 0,
            concurrency: 0,
            lane: Lane::Standard,
            features: HashMap::new(),
        }
    }
}

impl Entitlements {
    /// Parse from the `plans.entitlements` jsonb. Missing,
    /// mistyped, or negative numeric keys read as 0; an
    /// unknown or missing lane reads as `standard`; the
    /// `features` map keeps only boolean values (a non-bool
    /// entry is ignored, never treated as granted).
    pub fn from_value(value: &serde_json::Value) -> Self {
        let num = |key: &str| -> i64 {
            value
                .get(key)
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0)
                .max(0)
        };
        let lane = match value.get("lane").and_then(serde_json::Value::as_str) {
            Some("priority") => Lane::Priority,
            _ => Lane::Standard,
        };
        let features = value
            .get("features")
            .and_then(serde_json::Value::as_object)
            .map(|obj| {
                obj.iter()
                    .filter_map(|(key, val)| val.as_bool().map(|flag| (key.clone(), flag)))
                    .collect()
            })
            .unwrap_or_default();
        Entitlements {
            daily_images: num("daily_images"),
            daily_video_seconds: num("daily_video_seconds"),
            max_bytes: num("max_bytes"),
            result_ttl_hours: num("result_ttl_hours"),
            concurrency: num("concurrency"),
            lane,
            features,
        }
    }

    /// Whether the named feature is granted (`true` in the
    /// `features` map). Unknown features are not granted.
    pub fn has_feature(&self, name: &str) -> bool {
        self.features.get(name).copied().unwrap_or(false)
    }

    /// Whether the named gated feature is granted (P1-055,
    /// FR-P-01). The four gated names are `api_keys`,
    /// `webhooks`, `batch` — read from the `features` map —
    /// and `priority_lane`, which is NOT a map entry: it reads
    /// the plan's `lane` (`priority` grants it, `standard`
    /// does not). A `priority_lane: true` map entry can never
    /// grant the lane — the lane is the only source — and an
    /// unknown name is never granted.
    pub fn grants(&self, name: &str) -> bool {
        if name == "priority_lane" {
            return self.lane == Lane::Priority;
        }
        self.has_feature(name)
    }
}

/// The repo interface: the `plans` table, cheapest first.
pub trait PlanStore: Send + Sync {
    /// All plans, cheapest first (by `amount`).
    fn all(&self) -> impl std::future::Future<Output = Result<Vec<Plan>, RepoError>> + Send;
}

/// The sqlx `PlanStore`: [`crate::repos::plans::all`] over a
/// pool.
#[derive(Debug, Clone)]
pub struct SqlxPlanStore {
    pool: DbPool,
}

impl SqlxPlanStore {
    /// Build over a pool.
    pub fn new(pool: DbPool) -> Self {
        SqlxPlanStore { pool }
    }
}

impl PlanStore for SqlxPlanStore {
    async fn all(&self) -> Result<Vec<Plan>, RepoError> {
        crate::repos::plans::all(&self.pool).await
    }
}

/// `Arc` forwards to the inner store, so catalogs can be
/// shared (the api holds `Arc<PlanCatalog<...>>`).
impl<T: PlanStore> PlanStore for std::sync::Arc<T> {
    async fn all(&self) -> Result<Vec<Plan>, RepoError> {
        self.as_ref().all().await
    }
}

/// The cache interface: one TTL entry holding the whole
/// catalog + invalidation. The Redis implementation plugs
/// in behind this trait (P1-056's admin edit path).
pub trait PlanCache: Send + Sync {
    /// The cached catalog, if present and not expired.
    fn get(&self) -> Option<Vec<Plan>>;
    /// Cache the catalog for `ttl`.
    fn set(&self, plans: Vec<Plan>, ttl: Duration);
    /// Drop the cached catalog (admin edit path).
    fn invalidate(&self);
}

/// In-memory TTL cache. Tests and local dev only — production
/// uses Redis (NFR-10).
#[derive(Debug)]
pub struct InMemoryPlanCache {
    entry: Mutex<Option<(Vec<Plan>, Instant)>>,
}

impl InMemoryPlanCache {
    /// Empty cache.
    pub fn new() -> Self {
        InMemoryPlanCache {
            entry: Mutex::new(None),
        }
    }
}

impl Default for InMemoryPlanCache {
    fn default() -> Self {
        Self::new()
    }
}

impl PlanCache for InMemoryPlanCache {
    fn get(&self) -> Option<Vec<Plan>> {
        let mut entry = self.entry.lock().unwrap();
        match entry.as_ref() {
            Some((plans, expires_at)) if *expires_at > Instant::now() => Some(plans.clone()),
            Some(_) => {
                *entry = None;
                None
            }
            None => None,
        }
    }

    fn set(&self, plans: Vec<Plan>, ttl: Duration) {
        *self.entry.lock().unwrap() = Some((plans, Instant::now() + ttl));
    }

    fn invalidate(&self) {
        *self.entry.lock().unwrap() = None;
    }
}

/// `Arc` forwards to the inner cache, so catalogs can be
/// shared.
impl<T: PlanCache> PlanCache for std::sync::Arc<T> {
    fn get(&self) -> Option<Vec<Plan>> {
        self.as_ref().get()
    }

    fn set(&self, plans: Vec<Plan>, ttl: Duration) {
        self.as_ref().set(plans, ttl);
    }

    fn invalidate(&self) {
        self.as_ref().invalidate();
    }
}

/// The catalog: reads through the store, caches for `ttl`,
/// invalidates on edit. The 5-minute default is
/// [`PLAN_CACHE_TTL`].
#[derive(Debug)]
pub struct PlanCatalog<S, C> {
    store: S,
    cache: C,
    ttl: Duration,
}

impl<S: PlanStore, C: PlanCache> PlanCatalog<S, C> {
    /// Store + cache with the default 5-minute TTL.
    pub fn new(store: S, cache: C) -> Self {
        Self::with_ttl(store, cache, PLAN_CACHE_TTL)
    }

    /// Store + cache with an explicit TTL (tests use a short
    /// one).
    pub fn with_ttl(store: S, cache: C, ttl: Duration) -> Self {
        PlanCatalog { store, cache, ttl }
    }

    /// All plans, cheapest first — from the cache when fresh,
    /// else from the store (and then cached). A store failure
    /// is an error, never an empty catalog: an empty catalog
    /// would resolve every user to a missing Free plan.
    pub async fn catalog(&self) -> Result<Vec<Plan>, RepoError> {
        if let Some(plans) = self.cache.get() {
            return Ok(plans);
        }
        let plans = self.store.all().await?;
        self.cache.set(plans.clone(), self.ttl);
        Ok(plans)
    }

    /// One plan by code (`free` / `pro` / `business`).
    /// `None` when the code does not exist.
    pub async fn by_code(&self, code: &str) -> Result<Option<Plan>, RepoError> {
        Ok(self
            .catalog()
            .await?
            .into_iter()
            .find(|plan| plan.code == code))
    }

    /// One plan by id. `None` when the id does not exist
    /// (cannot happen for a live subscription — `plan_id`
    /// is `ON DELETE RESTRICT` — but the caller handles it
    /// as "no plan" rather than crashing).
    pub async fn by_id(&self, id: Uuid) -> Result<Option<Plan>, RepoError> {
        Ok(self.catalog().await?.into_iter().find(|plan| plan.id == id))
    }

    /// Drop the cached catalog (admin edit path — the next
    /// read reloads from the store).
    pub fn invalidate(&self) {
        self.cache.invalidate();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// The FR-P-01 seed entitlements, verbatim from migration
    /// 003 — the parse must reproduce them exactly.
    fn seed_entitlements(code: &str) -> serde_json::Value {
        match code {
            "free" => json!({
                "daily_images": 10,
                "daily_video_seconds": 0,
                "max_bytes": 26214400,
                "result_ttl_hours": 6,
                "concurrency": 1,
                "lane": "standard",
                "features": {}
            }),
            "pro" => json!({
                "daily_images": 500,
                "daily_video_seconds": 3600,
                "max_bytes": 1073741824,
                "result_ttl_hours": 24,
                "concurrency": 3,
                "lane": "standard",
                "features": {"api_keys": true, "webhooks": true}
            }),
            "business" => json!({
                "daily_images": 5000,
                "daily_video_seconds": 18000,
                "max_bytes": 4294967296i64,
                "result_ttl_hours": 72,
                "concurrency": 10,
                "lane": "priority",
                "features": {"api_keys": true, "webhooks": true, "batch": true}
            }),
            _ => unreachable!(),
        }
    }

    #[test]
    fn entitlements_parse_the_seed_values() {
        let free = Entitlements::from_value(&seed_entitlements("free"));
        assert_eq!(free.daily_images, 10);
        assert_eq!(free.daily_video_seconds, 0);
        assert_eq!(free.max_bytes, 26_214_400);
        assert_eq!(free.result_ttl_hours, 6);
        assert_eq!(free.concurrency, 1);
        assert_eq!(free.lane, Lane::Standard);
        assert!(!free.has_feature("api_keys"));

        let pro = Entitlements::from_value(&seed_entitlements("pro"));
        assert_eq!(pro.daily_images, 500);
        assert_eq!(pro.daily_video_seconds, 3600);
        assert_eq!(pro.max_bytes, 1_073_741_824);
        assert_eq!(pro.result_ttl_hours, 24);
        assert_eq!(pro.concurrency, 3);
        assert_eq!(pro.lane, Lane::Standard);
        assert!(pro.has_feature("api_keys"));
        assert!(pro.has_feature("webhooks"));
        assert!(!pro.has_feature("batch"));

        let business = Entitlements::from_value(&seed_entitlements("business"));
        assert_eq!(business.daily_images, 5000);
        assert_eq!(business.daily_video_seconds, 18000);
        // 4 GiB — past i32::MAX, so the struct must be i64.
        assert_eq!(business.max_bytes, 4_294_967_296);
        assert_eq!(business.result_ttl_hours, 72);
        assert_eq!(business.concurrency, 10);
        assert_eq!(business.lane, Lane::Priority);
        assert!(business.has_feature("batch"));
    }

    #[test]
    fn grants_covers_all_four_gated_features() {
        // P1-055: the four gated names, per the seeded plans
        // (FR-P-01) — Free grants nothing, Pro the key +
        // webhook features, Business all four.
        let free = Entitlements::from_value(&seed_entitlements("free"));
        for name in ["api_keys", "webhooks", "batch", "priority_lane"] {
            assert!(!free.grants(name), "free must not grant {name}");
        }

        let pro = Entitlements::from_value(&seed_entitlements("pro"));
        assert!(pro.grants("api_keys"));
        assert!(pro.grants("webhooks"));
        assert!(!pro.grants("batch"));
        assert!(!pro.grants("priority_lane"));

        let business = Entitlements::from_value(&seed_entitlements("business"));
        for name in ["api_keys", "webhooks", "batch", "priority_lane"] {
            assert!(business.grants(name), "business must grant {name}");
        }

        // Unknown names are never granted, even on Business.
        assert!(!business.grants("teleport"));

        // `priority_lane` reads the lane, never the map: a
        // standard-lane plan with the map entry set still does
        // not grant it, and a priority-lane plan grants it
        // with no map entry at all.
        let lying = Entitlements::from_value(&json!({
            "lane": "standard",
            "features": {"priority_lane": true}
        }));
        assert!(!lying.grants("priority_lane"));
        let honest = Entitlements::from_value(&json!({"lane": "priority"}));
        assert!(honest.grants("priority_lane"));
    }

    #[test]
    fn entitlements_fail_closed_on_bad_json() {
        // Missing keys read as 0 / standard / no features.
        let sparse = Entitlements::from_value(&json!({"daily_images": 5}));
        assert_eq!(sparse.daily_images, 5);
        assert_eq!(sparse.daily_video_seconds, 0);
        assert_eq!(sparse.max_bytes, 0);
        assert_eq!(sparse.result_ttl_hours, 0);
        assert_eq!(sparse.concurrency, 0);
        assert_eq!(sparse.lane, Lane::Standard);
        assert!(!sparse.has_feature("api_keys"));

        // Mistyped numbers read as 0 (a string is not a quota).
        let mistyped = Entitlements::from_value(&json!({
            "daily_images": "unlimited",
            "daily_video_seconds": 1.5,
            "max_bytes": true,
            "result_ttl_hours": null,
            "concurrency": -3,
            "lane": "express",
            "features": {"api_keys": "yes", "webhooks": true}
        }));
        assert_eq!(mistyped.daily_images, 0);
        assert_eq!(mistyped.daily_video_seconds, 0);
        assert_eq!(mistyped.max_bytes, 0);
        assert_eq!(mistyped.result_ttl_hours, 0);
        // A negative concurrency is clamped to 0, not -3.
        assert_eq!(mistyped.concurrency, 0);
        // An unknown lane is standard, never priority.
        assert_eq!(mistyped.lane, Lane::Standard);
        // A non-bool feature value is ignored, never granted.
        assert!(!mistyped.has_feature("api_keys"));
        assert!(mistyped.has_feature("webhooks"));

        // A non-object features value is no features at all.
        let bad_features = Entitlements::from_value(&json!({"features": ["api_keys"]}));
        assert!(!bad_features.has_feature("api_keys"));

        // The default is the fail-closed value.
        assert_eq!(
            Entitlements::default(),
            Entitlements::from_value(&json!({}))
        );
    }

    /// A store wrapper that counts `all` calls, proving the
    /// cache is hit. Rows are built in code (no DB here); the
    /// rows live behind a mutex so a test can flip them
    /// behind the catalog's back (another process / direct
    /// SQL).
    struct CountingStore {
        rows: Mutex<Vec<Plan>>,
        calls: AtomicUsize,
    }

    impl CountingStore {
        fn new(rows: Vec<Plan>) -> Self {
            CountingStore {
                rows: Mutex::new(rows),
                calls: AtomicUsize::new(0),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl PlanStore for CountingStore {
        async fn all(&self) -> Result<Vec<Plan>, RepoError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.rows.lock().unwrap().clone())
        }
    }

    fn plan_row(code: &str, amount: i32) -> Plan {
        Plan {
            id: Uuid::now_v7(),
            code: code.to_string(),
            stripe_price_id: None,
            razorpay_plan_id: None,
            currency: "INR".to_string(),
            amount,
            entitlements: seed_entitlements(code),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn catalog_caches_until_invalidated() {
        let store = CountingStore::new(vec![plan_row("free", 0), plan_row("pro", 49900)]);
        let catalog = PlanCatalog::new(store, InMemoryPlanCache::new());
        let first = catalog.catalog().await.expect("catalog reads");
        assert_eq!(first.len(), 2);
        let second = catalog.catalog().await.expect("catalog reads again");
        assert_eq!(second, first);
        assert_eq!(catalog.store.call_count(), 1, "second read must hit cache");
        catalog.invalidate();
        catalog
            .catalog()
            .await
            .expect("catalog reads after invalidate");
        assert_eq!(
            catalog.store.call_count(),
            2,
            "read after invalidate hits store"
        );
    }

    #[tokio::test]
    async fn catalog_by_code_and_by_id_read_through_cache() {
        let pro = plan_row("pro", 49900);
        let pro_id = pro.id;
        let store = CountingStore::new(vec![plan_row("free", 0), pro]);
        let catalog = PlanCatalog::new(store, InMemoryPlanCache::new());
        let found = catalog.by_code("pro").await.expect("by_code reads");
        assert_eq!(found.map(|plan| plan.id), Some(pro_id));
        assert_eq!(
            catalog
                .by_id(pro_id)
                .await
                .expect("by_id reads")
                .map(|plan| plan.code),
            Some("pro".to_string())
        );
        assert!(catalog
            .by_code("no-such-plan")
            .await
            .expect("unknown code reads")
            .is_none());
        // All three reads served from one store call.
        assert_eq!(catalog.store.call_count(), 1);
    }

    /// AC: an edit made behind the catalog's back (another
    /// process / direct SQL) is visible once the TTL expires.
    /// Short TTL + generous sleep margin (100 ms TTL, 200 ms
    /// sleep) so the test is stable.
    #[tokio::test]
    async fn catalog_flip_visible_within_ttl() {
        let store = Arc::new(CountingStore::new(vec![plan_row("free", 0)]));
        let cache = Arc::new(InMemoryPlanCache::new());
        let catalog = PlanCatalog::with_ttl(
            Arc::clone(&store),
            Arc::clone(&cache),
            Duration::from_millis(100),
        );
        assert_eq!(catalog.catalog().await.expect("read").len(), 1);
        // Flip behind the catalog's back: the store now holds
        // a second plan (no invalidation).
        store.rows.lock().unwrap().push(plan_row("pro", 49900));
        // Still cached: the old catalog shows until the TTL
        // expires.
        assert_eq!(catalog.catalog().await.expect("read").len(), 1);
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(
            catalog.catalog().await.expect("read").len(),
            2,
            "flip visible after TTL"
        );
    }
}
