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
//! - public: `PF_API_HOST:PF_API_PORT` (default 127.0.0.1:8080) — the api.
//! - metrics: `PF_METRICS_HOST:PF_METRICS_PORT` (default 127.0.0.1:9090) — the
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
//! | 2 | `request_id` middleware | Assigns/propagates `X-Request-Id` before anything else runs, so every log line, every error body, and every preflight response in the request carries the same id. It also creates the `http_request` tracing span — that span IS the trace layer (P0-065): tower-http's `TraceLayer` would create a second span per request with the same fields minus the request id, doubling log volume for no information. The `trace` feature stays available in the workspace for a future need, but it is not layered. |
//! | 3 | CORS (`CorsLayer`, `PF_WEB_ORIGIN`) | Outside the handlers so preflight `OPTIONS` is answered before auth, body limits, or routing — a preflight carries no credentials and must not need them. Inside request-id so preflights still get an id. A preflight short-circuits here, so it carries ACAO + `X-Request-Id` but NOT the secure headers (those are inner) — the order test asserts exactly that. |
//! | 4 | `docs_csp` middleware | Replaces the CSP with the docs CSP on `/docs` responses only (P0-102) — the Swagger UI needs same-origin scripts/styles, which the api's `default-src 'none'` would block. It sits OUTSIDE the secure-headers layer so its replacement wins on the way out; every other path passes through untouched. |
//! | 5 | `secure_headers` middleware | Stamps the header table (nosniff, `X-Frame-Options`, `Referrer-Policy`, CSP) on every response the inner layers produce — stubs, mapped errors, 404s — plus HSTS when the deployment is TLS-terminated (stage/prod, P0-102). Outside the error mapper so a rewritten body still gets the headers. |
//! | 6 | `http_metrics` middleware | The request timer starts here, so it covers the error mapper, the body-limit rejections, and the handler. If it wrapped only the handler, middleware time would be invisible to the latency metrics. |
//! | 7 | `error_mapper` middleware | Innermost middleware: it rewrites framework 400/404/413/415/422 responses into problem+json. It must sit INSIDE the body-limit layer (which lives per-module in `routes::api_router`, P0-065) so it sees the 413 the limit generates, and inside CORS/secure-headers/request-id so the rewritten response still gets ACAO, the secure-header table, and `X-Request-Id` on its way out. |
//! | 8 | Routes + `State<AppState>` | Innermost: handlers. State is not a layer — `with_state` attaches it to the router, so handlers can extract it regardless of layer position. The body-limit layer is applied per-module here (every module except uploads — see `routes::api_router`). |
//!
//! Principles for the next layers: a layer can only see
//! state/extensions added outside it; the timer must wrap
//! everything it measures; the request id must be assigned
//! before anything logs; an error response must leave through
//! the same outer layers a success does (CORS, secure headers,
//! request id), so the mapper is innermost.

pub mod auth;
pub mod body_limit;
pub mod cors;
pub mod error_mapper;
pub mod http_metrics;
pub mod metrics;
pub mod openapi;
pub mod problem;
pub mod request_id;
pub mod routes;
pub mod secure_headers;

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
/// The layer order is documented at the top of this file — read
/// it before adding a layer.
pub fn build_router(state: AppState, metrics: Metrics) -> Router {
    // CORS needs the origin before the state moves into the router.
    let web_origin = state.settings.web_origin.clone();
    // The Swagger UI (/docs) is dev+stage only (P0-067); the spec
    // document (/openapi.json, a meta route) is public in prod too.
    let docs_enabled = state.settings.docs_enabled();
    // The secure-headers middleware needs the state (it gates
    // HSTS on the deployment environment, P0-102) — clone it
    // before `with_state` moves it into the router.
    let headers_state = state.clone();
    let mut router = routes::api_router();
    if docs_enabled {
        router = router.merge(openapi::docs_router());
    }
    router
        .with_state(state)
        .layer(middleware::from_fn(error_mapper::error_mapper_middleware))
        .layer(middleware::from_fn(http_metrics::http_metrics_middleware))
        .layer(middleware::from_fn_with_state(
            headers_state,
            secure_headers::secure_headers_middleware,
        ))
        // The docs CSP override sits OUTSIDE the secure-headers
        // layer (P0-102): on the way out it runs after the api
        // table is stamped, so its replacement CSP wins on /docs.
        .layer(middleware::from_fn(secure_headers::docs_csp_middleware))
        .layer(cors::cors_layer(&web_origin))
        .layer(middleware::from_fn(request_id::request_id_middleware))
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
