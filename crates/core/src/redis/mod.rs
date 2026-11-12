//! Redis (P0-046, P0-047): health probe, connection manager, key helpers.
//!
//! - [`redis_health`] — the `/readyz` redis component. Deliberately
//!   client-free: it speaks raw RESP over a TCP stream (`PING` →
//!   `+PONG`) instead of going through a redis client, so readiness
//!   can check redis before the connection manager exists.
//! - [`create_pool`] — the deadpool-redis connection manager
//!   (P0-047): one pool per process, checked out per operation.
//! - [`keys`] — the `pf:{ns}:{key}` naming convention + TTL constants.
//! - [`ratelimit`] — fixed-window + token-bucket primitives
//!   (P0-048), atomic Lua scripts over the pool.
//! - [`cache`] — `get_or_set` with TTL + singleflight +
//!   invalidation (P0-049), with hit/miss metrics.
//! - [`pubsub`] — SSE pubsub: one Redis channel per job id
//!   (P0-050), publisher + bounded subscriber stream.
//!
//! Only the `redis://` scheme is probed (plain TCP). A `rediss://`
//! URL reports down with an explicit detail — TLS needs the client,
//! not this probe.

pub mod cache;
pub mod keys;
pub mod pubsub;
pub mod ratelimit;

use crate::db::ComponentHealth;
use crate::telemetry::redact;
use std::time::Duration;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

/// A deadpool-redis connection pool: one per process, checked out
/// per operation (`pool.get().await`).
pub type RedisPool = deadpool_redis::Pool;

/// Pool sizing + timeouts (P0-047).
#[derive(Debug, Clone)]
pub struct RedisPoolConfig {
    /// Maximum connections in the pool.
    pub max_size: usize,
    /// How long checkout waits for a free connection.
    pub wait_timeout: Duration,
    /// How long creating a new connection may take.
    pub create_timeout: Duration,
    /// How long a connection may sit idle before being recycled
    /// (`None` = never recycle on idle).
    pub recycle_timeout: Option<Duration>,
}

impl Default for RedisPoolConfig {
    fn default() -> Self {
        RedisPoolConfig {
            max_size: 16,
            wait_timeout: Duration::from_secs(2),
            create_timeout: Duration::from_secs(2),
            recycle_timeout: None,
        }
    }
}

/// Creating the pool failed. The display may contain the URL, so
/// never log this error unredacted (P0-032: `redact()` first).
#[derive(Debug, Error)]
pub enum RedisPoolError {
    /// The URL is malformed or the pool cannot be built from it.
    #[error("invalid redis URL or pool config: {0}")]
    CreatePool(#[from] deadpool_redis::CreatePoolError),
}

/// Build the connection pool for `redis_url` (P0-047).
/// Fails fast on a malformed URL; an unreachable server is NOT an
/// error here — deadpool connects lazily, and checkout reports it
/// (`PoolError`) instead. Pass `&RedisPoolConfig::default()` for
/// the dev defaults (16 connections, 2 s timeouts).
pub fn create_pool(redis_url: &str, config: &RedisPoolConfig) -> Result<RedisPool, RedisPoolError> {
    let mut cfg = deadpool_redis::Config::from_url(redis_url);
    cfg.pool = Some(deadpool_redis::PoolConfig {
        max_size: config.max_size,
        timeouts: deadpool_redis::Timeouts {
            wait: Some(config.wait_timeout),
            create: Some(config.create_timeout),
            recycle: config.recycle_timeout,
        },
        ..Default::default()
    });
    Ok(cfg.create_pool(Some(deadpool_redis::Runtime::Tokio1))?)
}

/// Health probe for the `/readyz` redis component: TCP connect +
/// `PING`, bounded by a 2 s timeout so a stuck server cannot hang
/// readiness (same bound as the db probe). `ok` is true only when
/// the server answers `+PONG`. The detail never contains the URL
/// or credentials.
pub async fn redis_health(redis_url: &str) -> ComponentHealth {
    let started = std::time::Instant::now();
    let probe = async {
        let url = url::Url::parse(redis_url).map_err(|_| "invalid redis URL".to_string())?;
        if url.scheme() != "redis" {
            return Err(format!("unsupported scheme: {}", url.scheme()));
        }
        let host = url
            .host_str()
            .ok_or_else(|| "redis URL has no host".to_string())?;
        let port = url.port().unwrap_or(6379);
        let mut stream = TcpStream::connect((host, port))
            .await
            .map_err(|err| err.to_string())?;
        stream
            .write_all(b"PING\r\n")
            .await
            .map_err(|err| err.to_string())?;
        let mut reader = BufReader::new(&mut stream);
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .await
            .map_err(|err| err.to_string())?;
        if line.trim_end() == "+PONG" {
            Ok(())
        } else {
            Err(format!("unexpected PING reply: {}", line.trim_end()))
        }
    };
    match tokio::time::timeout(Duration::from_secs(2), probe).await {
        Ok(Ok(())) => ComponentHealth::timed("redis", true, "ok".to_string(), started),
        Ok(Err(err)) => ComponentHealth::timed(
            "redis",
            false,
            format!("unavailable: {}", redact(&err)),
            started,
        ),
        Err(_) => ComponentHealth::timed(
            "redis",
            false,
            "unavailable: probe timed out".to_string(),
            started,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;
    use tokio::net::TcpListener;

    /// A fake redis: accepts one connection, reads one line, replies
    /// with `reply`, then closes. Returns the port to probe.
    async fn fake_redis(reply: &'static [u8]) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fake redis");
        let port = listener.local_addr().expect("port").port();
        tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                let mut buf = [0u8; 64];
                let _ = stream.read(&mut buf).await;
                let _ = stream.write_all(reply).await;
            }
        });
        port
    }

    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .expect("bind :0 for a free port")
            .local_addr()
            .expect("port of :0 listener")
            .port()
    }

    #[tokio::test]
    async fn pong_is_healthy() {
        let port = fake_redis(b"+PONG\r\n").await;
        let health = redis_health(&format!("redis://127.0.0.1:{port}")).await;
        assert!(health.ok, "PONG must be healthy: {health:?}");
        assert_eq!(health.name, "redis");
        assert_eq!(health.detail, "ok");
        // P0-066: the probe reports its latency, bounded by the 2 s
        // timeout.
        assert!(
            health.latency_ms < 2000,
            "healthy probe latency must be under the timeout: {health:?}"
        );
    }

    #[tokio::test]
    async fn wrong_reply_is_down() {
        let port = fake_redis(b"-ERR unknown command\r\n").await;
        let health = redis_health(&format!("redis://127.0.0.1:{port}")).await;
        assert!(!health.ok, "an error reply must be down: {health:?}");
        assert!(health.detail.contains("unavailable"));
    }

    #[tokio::test]
    async fn closed_port_is_down() {
        let health = redis_health(&format!("redis://127.0.0.1:{}", free_port())).await;
        assert!(!health.ok, "a closed port must be down: {health:?}");
        assert!(health.detail.contains("unavailable"));
        // P0-066: a down component reports its latency too, never a
        // missing one.
        assert!(
            health.latency_ms < 2000,
            "down probe latency must be under the timeout: {health:?}"
        );
    }

    #[tokio::test]
    async fn non_redis_scheme_is_down() {
        let health = redis_health("http://127.0.0.1:6379").await;
        assert!(!health.ok, "http:// must be down: {health:?}");
        assert!(health.detail.contains("unsupported scheme"));
    }

    #[tokio::test]
    async fn db_index_in_url_is_ignored_for_ping() {
        let port = fake_redis(b"+PONG\r\n").await;
        let health = redis_health(&format!("redis://127.0.0.1:{port}/3")).await;
        assert!(health.ok, "a /db suffix must not break PING: {health:?}");
    }
}
