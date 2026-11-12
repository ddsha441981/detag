//! detag-api — Axum HTTP server (P0-063, E0.7).
//!
//! The router lives in the lib target so integration tests
//! (`apps/api/tests/`) can drive it without spawning the binary.
//! The binary (`main.rs`) only loads settings, builds the state,
//! and serves two listeners (see below).
//!
//! # State
//!
//! Every handler gets [`AppState`] (axum `State`): the Postgres
//! pool, the Redis pool, the storage backend, the settings, and
//! the clock. The state is built once in
//! [`AppState::from_settings`] — construction is lazy where it
//! can be (the Postgres pool and the Redis pool connect on first
//! use, so boot never fails on a temporarily-down dependency)
//! and fails fast where it must (a malformed URL or an invalid
//! storage config is a boot error, not a runtime surprise).
//! [`Metrics`] is deliberately NOT in the state: the metrics
//! registry is shared with the second listener's `/metrics`
//! router, so it travels as an `Extension` layer instead (see
//! the layer order).
//!
//! # Listeners (main.rs)
//!
//! Two listeners, one process:
//! - public: `127.0.0.1:PF_API_PORT` (default 8080) — the api.
//! - metrics: `127.0.0.1:PF_METRICS_PORT` (default 9090) — the
//!   internal `/metrics` server, never on the public port
//!   (ASSUMPTION-003). One signal listener stops both; both
//!   drain in-flight requests on SIGTERM/SIGINT (P0-023).
//!
//! (The ticket spec for this work said "metrics 8081"; the
//! shipped default is 9090 — P0-026, documented in the runbook
//! and the config reference — so 9090 stands. See the P0-063
//! status in `docs/07-PHASES/PHASE-0-foundation.md`.)
//!
//! # Tower layer order
//!
//! In tower, the LAST `.layer()` call is the OUTERMOST layer —
//! it runs first. `build_router` applies layers in this order
//! (outer → inner, i.e. the order a request meets them):
//!
//! | # | Layer | Why it sits here |
//! | - | ----- | ---------------- |
//! | 1 | `Extension<Metrics>` | Outermost so every inner layer can read it. A layer can only see extensions added OUTSIDE it: if this were innermost, the metrics middleware below could not find the registry (it panics with "http metrics Extension missing"). |
//! | 2 | `http_metrics` middleware | The request timer starts here, so it covers the request-id middleware AND the handler. If it wrapped only the handler, middleware time would be invisible to the latency metrics. |
//! | 3 | `request_id` middleware | Assigns/propagates `X-Request-Id` before the handler runs (and before any handler logging), so every log line and every error body in the request carries the same id. |
//! | 4 | Routes + `State<AppState>` | Innermost: handlers. State is not a layer — `with_state` attaches it to the router, so handlers (and `from_fn` middleware, if they ever need it) can extract it regardless of layer position. |
//!
//! Principles for the next layers (P0-065 adds trace, body
//! limit, CORS, secure headers, the error mapper): a layer can
//! only see state/extensions added outside it; the timer must
//! wrap everything it measures; the request id must be assigned
//! before anything logs. P0-065 extends this table — it does not
//! replace it.

pub mod http_metrics;
pub mod metrics;
pub mod request_id;
pub mod routes;

use axum::{extract::Extension, middleware, Router};
use detag_core::clock::{Clock, SystemClock};
use detag_core::config::Settings;
use detag_core::db::{connect_lazy, DbPool, PoolConfig};
use detag_core::metrics::Metrics;
use detag_core::redis::{create_pool, RedisPool, RedisPoolConfig, RedisPoolError};
use detag_storage::provider::StorageError;
use detag_storage::s3::{S3Config, S3Storage};
use std::sync::Arc;
use thiserror::Error;

/// The per-process state every handler gets (P0-063).
#[derive(Clone, Debug)]
pub struct AppState {
    /// Postgres pool (lazy — boot never fails on a down database).
    pub pool: DbPool,
    /// Redis pool (lazy — checkout reports a down server).
    pub redis: RedisPool,
    /// Object storage (validates config only; no network call).
    pub storage: S3Storage,
    /// The loaded settings (ports, URLs, TTLs, flags).
    pub settings: Arc<Settings>,
    /// Time source: [`SystemClock`] in production, a frozen
    /// [`ManualClock`](detag_core::clock::ManualClock) in tests.
    pub clock: Arc<dyn Clock>,
}

/// Building the state failed. The `Display` is deliberately a
/// static string per failure — the inner errors can contain a
/// URL (redis) or endpoint details, so they are never echoed.
/// The inner error stays available via `source()` for logs that
/// redact first (`detag_core::telemetry::redact`).
#[derive(Debug, Error)]
pub enum AppStateError {
    /// `PF_DATABASE_URL` is malformed.
    #[error("invalid PF_DATABASE_URL")]
    Database(#[from] sqlx::Error),

    /// `PF_REDIS_URL` is malformed.
    #[error("invalid PF_REDIS_URL")]
    Redis(#[from] RedisPoolError),

    /// A `PF_S3_*` value is missing or unusable.
    #[error("invalid storage configuration (PF_S3_*)")]
    Storage(#[from] StorageError),
}

impl AppState {
    /// Build the state from loaded settings. Fails fast only on
    /// malformed configuration (see [`AppStateError`]); a
    /// temporarily-down Postgres/Redis/MinIO is NOT an error here
    /// — the pools connect lazily and `/readyz` reports it.
    pub fn from_settings(settings: &Settings) -> Result<Self, AppStateError> {
        let pool = connect_lazy(&settings.database_url, &PoolConfig::default())?;
        let redis = create_pool(&settings.redis_url, &RedisPoolConfig::default())?;
        let storage = S3Storage::new(S3Config {
            endpoint: settings.s3_endpoint.clone(),
            bucket: settings.s3_bucket.clone(),
            region: settings.s3_region.clone(),
            access_key: settings.s3_access_key.clone(),
            secret_key: settings.s3_secret_key.clone(),
        })?;
        Ok(Self {
            pool,
            redis,
            storage,
            settings: Arc::new(settings.clone()),
            clock: Arc::new(SystemClock),
        })
    }

    /// Replace the clock (tests: freeze time). The state is
    /// `Clone`, so this is cheap — it clones the `Arc`s.
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }
}

/// The router: the route modules (`routes/`, P0-064) merged,
/// with the state attached and the middleware layered on top.
/// The remaining middleware lands in P0-065. The layer order is
/// documented at the top of this file — read it before adding a
/// layer.
pub fn build_router(state: AppState, metrics: Metrics) -> Router {
    routes::api_router()
        .with_state(state)
        .layer(middleware::from_fn(request_id::request_id_middleware))
        .layer(middleware::from_fn(http_metrics::http_metrics_middleware))
        .layer(Extension(metrics))
}

#[cfg(test)]
mod tests {
    use super::*;
    use detag_core::clock::ManualClock;

    fn test_settings() -> Settings {
        Settings::default()
    }

    #[tokio::test]
    async fn router_builds_with_full_state() {
        // Lazy pools + config-only storage: the router builds with
        // no database, redis, or MinIO behind the defaults.
        let state = AppState::from_settings(&test_settings()).expect("default settings build");
        let _app = build_router(state, Metrics::new());
    }

    #[tokio::test]
    async fn state_clock_is_replaceable() {
        let frozen = ManualClock::new(
            chrono::DateTime::parse_from_rfc3339("2026-11-12T00:00:00Z")
                .expect("fixture parses")
                .with_timezone(&chrono::Utc),
        );
        let state = AppState::from_settings(&test_settings())
            .expect("default settings build")
            .with_clock(Arc::new(frozen));
        assert_eq!(
            state.clock.now(),
            chrono::DateTime::parse_from_rfc3339("2026-11-12T00:00:00Z")
                .expect("fixture parses")
                .with_timezone(&chrono::Utc),
            "the frozen clock must be what handlers see"
        );
    }

    #[tokio::test]
    async fn malformed_redis_url_fails_boot() {
        let mut settings = test_settings();
        settings.redis_url = "not a url".to_string();
        assert!(
            matches!(
                AppState::from_settings(&settings),
                Err(AppStateError::Redis(_))
            ),
            "a malformed redis URL must fail state construction"
        );
    }

    #[tokio::test]
    async fn malformed_storage_endpoint_fails_boot() {
        let mut settings = test_settings();
        settings.s3_endpoint = "not a url".to_string();
        assert!(
            matches!(
                AppState::from_settings(&settings),
                Err(AppStateError::Storage(_))
            ),
            "a malformed S3 endpoint must fail state construction"
        );
    }

    #[tokio::test]
    async fn state_error_display_leaks_nothing() {
        // The redis error's inner Display can contain the URL;
        // the AppStateError Display must be the static string.
        let mut settings = test_settings();
        settings.redis_url = "http://user:secret@example.com:6379".to_string();
        let err = AppState::from_settings(&settings).expect_err("must fail");
        let rendered = err.to_string();
        assert_eq!(rendered, "invalid PF_REDIS_URL");
        assert!(
            !rendered.contains("secret"),
            "no credentials in the message"
        );
        assert!(!rendered.contains("example.com"), "no host in the message");
    }
}
