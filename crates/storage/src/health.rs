//! Storage health probe for the `/readyz` storage component
//! (P0-066, FR-X-02).
//!
//! `HeadBucket` against the configured bucket, bounded by a 2 s
//! timeout (the same bound as the db/redis probes) so a stuck
//! backend cannot hang readiness. `ok` is true only when the
//! backend answers 200 — a missing bucket is down, not "ok": the
//! bucket must exist (ops creates it, P0-054) before the api can
//! serve uploads.
//!
//! The detail is PII-free by construction:
//! [`crate::provider::StorageError`]'s `Display` never echoes the
//! backend's message (see `provider.rs`), so the probe's error
//! text is safe to expose. No credentials ever reach the body —
//! the secret key lives only inside the SDK client.

use std::time::Duration;

use detag_core::db::ComponentHealth;

use crate::s3::S3Storage;

/// Health probe for the `/readyz` storage component: `HeadBucket`
/// on the configured bucket. Down covers a missing bucket, bad
/// credentials, and an unreachable endpoint alike — the detail
/// says which class failed without ever echoing provider text.
pub async fn storage_health(storage: &S3Storage) -> ComponentHealth {
    let started = std::time::Instant::now();
    let probe = storage.check_bucket();
    match tokio::time::timeout(Duration::from_secs(2), probe).await {
        Ok(Ok(())) => ComponentHealth::timed("storage", true, "ok".to_string(), started),
        Ok(Err(err)) => {
            ComponentHealth::timed("storage", false, format!("unavailable: {err}"), started)
        }
        Err(_) => ComponentHealth::timed(
            "storage",
            false,
            "unavailable: probe timed out".to_string(),
            started,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::s3::S3Config;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn test_config(endpoint: String) -> S3Config {
        S3Config {
            endpoint,
            bucket: "detag-dev".to_string(),
            region: "us-east-1".to_string(),
            access_key: "test-access-key".to_string(),
            // A distinctive secret: the down-path test asserts it
            // never appears in the health detail.
            secret_key: "test-secret-xyz-123".to_string(),
        }
    }

    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .expect("bind :0 for a free port")
            .local_addr()
            .expect("port of :0 listener")
            .port()
    }

    /// A fake S3: accepts one connection, reads the request head,
    /// replies with `response`, then closes. Returns the port.
    async fn fake_s3(response: &'static [u8]) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fake s3");
        let port = listener.local_addr().expect("port").port();
        tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                // Read until the end of the request head so the
                // client has finished sending before we answer.
                let mut buf = [0u8; 4096];
                let mut got = 0;
                while got < buf.len() {
                    match stream.read(&mut buf[got..]).await {
                        Ok(0) => break,
                        Ok(n) => {
                            got += n;
                            if buf[..got].windows(4).any(|w| w == b"\r\n\r\n") {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
                let _ = stream.write_all(response).await;
                let _ = stream.flush().await;
            }
        });
        port
    }

    #[tokio::test]
    async fn head_bucket_200_is_healthy() {
        let port = fake_s3(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").await;
        let storage = S3Storage::new(test_config(format!("http://127.0.0.1:{port}")))
            .expect("test config builds");
        let health = storage_health(&storage).await;
        assert!(health.ok, "HeadBucket 200 must be healthy: {health:?}");
        assert_eq!(health.name, "storage");
        assert_eq!(health.detail, "ok");
        // P0-066: the probe reports its latency, bounded by the
        // 2 s timeout.
        assert!(
            health.latency_ms < 2000,
            "healthy probe latency must be under the timeout: {health:?}"
        );
    }

    #[tokio::test]
    async fn missing_bucket_is_down() {
        // A real S3 404 with an error body: the bucket does not
        // exist, so readiness must report storage down.
        let body = br#"<?xml version="1.0" encoding="UTF-8"?><Error><Code>NoSuchBucket</Code><Message>The specified bucket does not exist</Message></Error>"#;
        let mut response = format!(
            "HTTP/1.1 404 Not Found\r\nContent-Type: application/xml\r\nContent-Length: {}\r\n\r\n",
            body.len()
        )
        .into_bytes();
        response.extend_from_slice(body);
        let response: &'static [u8] = Box::leak(response.into_boxed_slice());
        let port = fake_s3(response).await;
        let storage = S3Storage::new(test_config(format!("http://127.0.0.1:{port}")))
            .expect("test config builds");
        let health = storage_health(&storage).await;
        assert!(!health.ok, "a missing bucket must be down: {health:?}");
        assert!(health.detail.contains("unavailable"));
        assert!(
            health.latency_ms < 2000,
            "down probe latency must be under the timeout: {health:?}"
        );
    }

    #[tokio::test]
    async fn closed_port_is_down_and_leaks_no_secret() {
        let storage = S3Storage::new(test_config(format!("http://127.0.0.1:{}", free_port())))
            .expect("test config builds");
        let health = storage_health(&storage).await;
        assert!(!health.ok, "a closed port must be down: {health:?}");
        assert!(health.detail.contains("unavailable"));
        assert!(
            health.latency_ms < 2000,
            "down probe latency must be under the timeout: {health:?}"
        );
        assert!(
            !health.detail.contains("test-secret-xyz-123"),
            "secret key must not leak into the detail: {}",
            health.detail
        );
        assert!(
            !health.detail.contains("test-access-key"),
            "access key must not leak into the detail: {}",
            health.detail
        );
    }
}
