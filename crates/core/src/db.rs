//! detag-core::db — Postgres pool + readiness component (P0-035, E0.4).
//!
//! Both the api and the worker build their pool through here, so the
//! settings (max connections, acquire timeout, statement cache) live
//! in exactly one place. The pool is [`DbPool`] (`sqlx::PgPool`).
//!
//! Queries use the untyped sqlx API (`query_scalar`, …), so this
//! module needs no `sqlx prepare` offline data. Repositories that
//! use the `query!` macros (Phase 1+) check their `.sqlx/` outputs
//! in, starting with P0-037.
//!
//! TLS: no sqlx TLS feature is enabled, so connections are plaintext.
//! That matches the dev compose service (no TLS) and a database on a
//! private network. If production ever needs TLS to Postgres, enable
//! a `tls-*` sqlx feature and revisit — do not silently assume it.

use crate::telemetry::redact;
use serde::Serialize;
use std::time::Duration;

/// The shared Postgres pool type (re-exported so the apps never name
/// sqlx directly).
pub type DbPool = sqlx::PgPool;

/// Pool settings. Defaults are for local dev / a small single VPS;
/// tune when the load profile is known (Phase 5).
#[derive(Debug, Clone)]
pub struct PoolConfig {
    /// Max open connections (api + worker each build their own pool).
    pub max_connections: u32,
    /// Min idle connections held open (0 = open lazily).
    pub min_connections: u32,
    /// How long acquiring a connection may wait when the pool is full.
    pub acquire_timeout: Duration,
    /// sqlx prepared-statement cache capacity; 0 disables the cache.
    pub statement_cache_capacity: usize,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            max_connections: 10,
            min_connections: 0,
            acquire_timeout: Duration::from_secs(5),
            statement_cache_capacity: 100,
        }
    }
}

/// One readiness component: a name, up/down, and a safe detail
/// string. `detail` never contains the database URL or credentials.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ComponentHealth {
    pub name: &'static str,
    pub ok: bool,
    pub detail: String,
}

/// Overall readiness: `"ok"` only when every component is ok (and
/// at least one component was checked — an empty report is
/// `"degraded"`, never `"ok"`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Readiness {
    pub status: &'static str,
    pub components: Vec<ComponentHealth>,
}

impl Readiness {
    pub fn from_components(components: Vec<ComponentHealth>) -> Self {
        let ok = !components.is_empty() && components.iter().all(|c| c.ok);
        Self {
            status: if ok { "ok" } else { "degraded" },
            components,
        }
    }

    pub fn is_ok(&self) -> bool {
        self.status == "ok"
    }
}

/// Eager connect: parses the URL, opens the pool, and fails fast on a
/// malformed URL or an unreachable database. Use for tests and for
/// one-shot tools (migrations) where failing fast is right.
pub async fn connect(database_url: &str, config: &PoolConfig) -> Result<DbPool, sqlx::Error> {
    pool_options(config)
        .connect_with(connect_options(database_url, config)?)
        .await
}

/// Lazy connect: the same pool, but no connection is attempted until
/// first use. The mains use this so boot never fails on a
/// temporarily-down database; `/readyz` then reports the truth
/// (down) instead of the process dying. A malformed URL still fails
/// here — fail fast on a config error, not on a down dependency.
pub fn connect_lazy(database_url: &str, config: &PoolConfig) -> Result<DbPool, sqlx::Error> {
    Ok(pool_options(config).connect_lazy_with(connect_options(database_url, config)?))
}

fn pool_options(config: &PoolConfig) -> sqlx::postgres::PgPoolOptions {
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(config.max_connections)
        .min_connections(config.min_connections)
        .acquire_timeout(config.acquire_timeout)
}

/// The statement cache lives on the connect options, not the pool
/// options (sqlx 0.9): build both from the one config.
fn connect_options(
    database_url: &str,
    config: &PoolConfig,
) -> Result<sqlx::postgres::PgConnectOptions, sqlx::Error> {
    use std::str::FromStr;
    Ok(sqlx::postgres::PgConnectOptions::from_str(database_url)?
        .statement_cache_capacity(config.statement_cache_capacity))
}

/// Health probe for the `/readyz` db component: `SELECT 1`, bounded
/// by a 2 s timeout so a stuck pool cannot hang readiness. `ok` is
/// true only when the query returns exactly 1.
pub async fn db_health(pool: &DbPool) -> ComponentHealth {
    let probe = sqlx::query_scalar::<_, i32>("SELECT 1").fetch_one(pool);
    match tokio::time::timeout(Duration::from_secs(2), probe).await {
        Ok(Ok(1)) => ComponentHealth {
            name: "db",
            ok: true,
            detail: "ok".to_string(),
        },
        Ok(Ok(_)) => ComponentHealth {
            name: "db",
            ok: false,
            detail: "unexpected SELECT 1 result".to_string(),
        },
        Ok(Err(err)) => ComponentHealth {
            name: "db",
            ok: false,
            detail: format!("unavailable: {}", redact(&err.to_string())),
        },
        Err(_) => ComponentHealth {
            name: "db",
            ok: false,
            detail: "unavailable: probe timed out".to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pool_config_defaults() {
        let config = PoolConfig::default();
        assert_eq!(config.max_connections, 10);
        assert_eq!(config.min_connections, 0);
        assert_eq!(config.acquire_timeout, Duration::from_secs(5));
        assert_eq!(config.statement_cache_capacity, 100);
    }

    #[test]
    fn readiness_ok_only_when_every_component_ok() {
        let ok = || ComponentHealth {
            name: "db",
            ok: true,
            detail: "ok".to_string(),
        };
        let down = || ComponentHealth {
            name: "db",
            ok: false,
            detail: "unavailable".to_string(),
        };
        assert!(Readiness::from_components(vec![ok()]).is_ok());
        assert!(Readiness::from_components(vec![ok(), ok()]).is_ok());
        assert!(!Readiness::from_components(vec![ok(), down()]).is_ok());
        assert!(!Readiness::from_components(vec![down()]).is_ok());
        // No components checked is not "ok".
        assert!(!Readiness::from_components(vec![]).is_ok());
        assert_eq!(Readiness::from_components(vec![ok()]).status, "ok");
        assert_eq!(Readiness::from_components(vec![down()]).status, "degraded");
    }

    #[test]
    fn readiness_serializes_to_the_readyz_shape() {
        let report = Readiness::from_components(vec![ComponentHealth {
            name: "db",
            ok: true,
            detail: "ok".to_string(),
        }]);
        let json = serde_json::to_value(&report).expect("serializes");
        assert_eq!(json["status"], "ok");
        assert_eq!(json["components"][0]["name"], "db");
        assert_eq!(json["components"][0]["ok"], true);
        assert_eq!(json["components"][0]["detail"], "ok");
    }

    /// The down path needs no live database: a lazy pool pointed at a
    /// closed port reports down, and the detail never leaks the URL
    /// (which here carries a password).
    #[tokio::test]
    async fn db_health_down_on_unreachable_database() {
        let pool = connect_lazy(
            "postgres://pf:not-a-real-password@127.0.0.1:59999/pf",
            &PoolConfig::default(),
        )
        .expect("lazy connect only fails on a malformed URL");
        let health = db_health(&pool).await;
        assert!(!health.ok, "dead database must report down");
        assert_eq!(health.name, "db");
        assert!(
            health.detail.contains("unavailable"),
            "detail must say unavailable: {}",
            health.detail
        );
        assert!(
            !health.detail.contains("not-a-real-password"),
            "password must not leak into the detail: {}",
            health.detail
        );
        pool.close().await;
    }

    /// Eager connect fails fast on an unreachable database.
    #[tokio::test]
    async fn connect_fails_fast_on_unreachable_database() {
        let err = connect(
            "postgres://pf:pf@127.0.0.1:59999/pf",
            &PoolConfig::default(),
        )
        .await
        .expect_err("eager connect to a dead database must fail");
        assert!(
            !err.to_string().contains("pf@"),
            "error must not echo credentials: {err}"
        );
    }
}
