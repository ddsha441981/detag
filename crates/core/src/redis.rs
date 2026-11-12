//! Redis health probe (P0-046): the `/readyz` redis component.
//!
//! Deliberately client-free: it speaks raw RESP over a TCP stream
//! (`PING` → `+PONG`) instead of going through a redis client, so
//! readiness can check redis before the connection manager exists.
//! The real client (deadpool-redis pool, key helpers, TTLs) lands
//! in P0-047 and will live in this module next to the probe.
//!
//! Only the `redis://` scheme is probed (plain TCP). A `rediss://`
//! URL reports down with an explicit detail — TLS needs the client,
//! not this probe.

use crate::db::ComponentHealth;
use crate::telemetry::redact;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

/// Health probe for the `/readyz` redis component: TCP connect +
/// `PING`, bounded by a 2 s timeout so a stuck server cannot hang
/// readiness (same bound as the db probe). `ok` is true only when
/// the server answers `+PONG`. The detail never contains the URL
/// or credentials.
pub async fn redis_health(redis_url: &str) -> ComponentHealth {
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
        Ok(Ok(())) => ComponentHealth {
            name: "redis",
            ok: true,
            detail: "ok".to_string(),
        },
        Ok(Err(err)) => ComponentHealth {
            name: "redis",
            ok: false,
            detail: format!("unavailable: {}", redact(&err)),
        },
        Err(_) => ComponentHealth {
            name: "redis",
            ok: false,
            detail: "unavailable: probe timed out".to_string(),
        },
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
