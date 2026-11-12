//! P0-035: /readyz reflects db health — end to end, against the
//! spawned api binary. P0-046 adds the redis component the same way.
//! P0-066 adds storage + models the same way, and pins the
//! per-component `latency_ms` on every component.
//!
//! Up case: the binary boots with the default PF_DATABASE_URL (the
//! dev database — native postgres on this VM, compose postgres where
//! docker runs), the default PF_REDIS_URL (native redis on this
//! VM, compose redis where docker runs), and PF_S3_ENDPOINT
//! pointed at a fake S3 in this test (a minimal HTTP server that
//! answers 200 to `HeadBucket` — MinIO is not running on this VM,
//! and the storage live leg is covered by the storage crate's own
//! tests + CI) and /readyz answers 200 with all four components
//! ok.
//! Down cases: the binary boots with PF_DATABASE_URL (or
//! PF_REDIS_URL, or PF_S3_ENDPOINT) pointed at a closed port —
//! boot still succeeds (lazy pool, P0-035) and /readyz answers
//! 503 with that component down.

use std::process::{Child, Command, Stdio};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::net::TcpStream;
use tokio::time::{sleep, timeout};

/// A free port, found by binding :0 and releasing it.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind :0 for a free port")
        .local_addr()
        .expect("port of :0 listener")
        .port()
}

/// Minimal HTTP GET over a raw socket; returns (status line, body).
async fn http_get(port: u16, path: &str) -> std::io::Result<(String, String)> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await?;
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").as_bytes(),
        )
        .await?;
    let mut raw = Vec::new();
    timeout(Duration::from_secs(10), stream.read_to_end(&mut raw))
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "response timed out"))??;
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text
        .split_once("\r\n\r\n")
        .unwrap_or_else(|| panic!("no header/body split in response: {text}"));
    let status = head.lines().next().unwrap_or("").to_string();
    Ok((status, body.to_string()))
}

/// Poll /healthz until it answers 200 (panics after 10 s).
async fn wait_for_healthz(port: u16) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(Ok((status, _))) =
            timeout(Duration::from_secs(1), http_get(port, "/healthz")).await
        {
            if status.starts_with("HTTP/1.1 200") {
                return;
            }
        }
        if std::time::Instant::now() > deadline {
            panic!("api did not become healthy on port {port} within 10 s");
        }
        sleep(Duration::from_millis(100)).await;
    }
}

/// Wait for the child to exit (panics after 10 s, killing it first).
async fn wait_for_exit(child: &mut Child) -> std::process::ExitStatus {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status,
            Ok(None) => {
                if std::time::Instant::now() > deadline {
                    let _ = child.kill();
                    panic!("api did not exit within 10 s of SIGTERM");
                }
                sleep(Duration::from_millis(100)).await;
            }
            Err(err) => panic!("try_wait failed: {err}"),
        }
    }
}

fn spawn_api(
    api_port: u16,
    metrics_port: u16,
    database_url: Option<&str>,
    redis_url: Option<&str>,
    s3_endpoint: Option<&str>,
) -> Child {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_detag-api"));
    cmd.env("PF_API_PORT", api_port.to_string())
        .env("PF_METRICS_PORT", metrics_port.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(url) = database_url {
        cmd.env("PF_DATABASE_URL", url);
    }
    if let Some(url) = redis_url {
        cmd.env("PF_REDIS_URL", url);
    }
    if let Some(endpoint) = s3_endpoint {
        cmd.env("PF_S3_ENDPOINT", endpoint);
    }
    cmd.spawn().expect("spawn detag-api")
}

/// A fake S3 for the up case: answers `200 OK` to every request
/// (the binary only ever issues `HeadBucket` against it). Loops
/// accepting connections until the returned handle is aborted.
/// MinIO is not running on this VM (DOCKER-001), so the up case
/// cannot point at a real bucket here; the storage crate's own
/// tests cover the probe against a real MinIO in CI.
async fn fake_s3() -> (u16, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake s3");
    let port = listener.local_addr().expect("port").port();
    let handle = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
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
                let _ = stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                    .await;
                let _ = stream.flush().await;
            });
        }
    });
    (port, handle)
}

/// Every component in a /readyz body reports its probe latency
/// (P0-066): a number, bounded by the 2 s probe timeout. The bound
/// allows the timeout itself plus scheduling slack: the db probe
/// takes the whole 2 s on an unreachable database (sqlx retries
/// the acquire internally — ASSUMPTION-012), so the timeout arm
/// legitimately reports ~2000 ms.
fn assert_latencies(json: &serde_json::Value) {
    for c in json["components"]
        .as_array()
        .expect("components is an array")
    {
        let latency = c["latency_ms"]
            .as_u64()
            .unwrap_or_else(|| panic!("{} must report latency_ms: {json}", c["name"]));
        assert!(
            latency <= 2050,
            "{} latency must be bounded by the 2 s probe timeout: {latency} ms",
            c["name"]
        );
    }
}

/// The component with `name` in a /readyz JSON body.
fn component<'a>(json: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    json["components"]
        .as_array()
        .expect("components is an array")
        .iter()
        .find(|c| c["name"] == name)
        .unwrap_or_else(|| panic!("no {name} component in {json}"))
}

async fn stop_api(child: &mut Child) {
    let rc = unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
    assert_eq!(rc, 0, "SIGTERM must succeed");
    let status = wait_for_exit(child).await;
    assert!(
        status.success(),
        "api must exit 0 after SIGTERM, got {status}"
    );
}

#[tokio::test]
async fn readyz_ok_when_all_components_up() {
    let api_port = free_port();
    let metrics_port = free_port();
    let (s3_port, fake) = fake_s3().await;
    let mut child = spawn_api(
        api_port,
        metrics_port,
        None,
        None,
        Some(&format!("http://127.0.0.1:{s3_port}")),
    );
    wait_for_healthz(api_port).await;

    let (status, body) = http_get(api_port, "/readyz").await.expect("GET /readyz");
    assert!(
        status.starts_with("HTTP/1.1 200"),
        "readyz must be 200 with all components up, got {status}: {body}"
    );
    let json: serde_json::Value = serde_json::from_str(&body).expect("readyz body is JSON");
    assert_eq!(json["status"], "ok");
    let db = component(&json, "db");
    assert_eq!(db["ok"], true);
    assert_eq!(db["detail"], "ok");
    let redis = component(&json, "redis");
    assert_eq!(redis["ok"], true, "redis must be ok: {body}");
    assert_eq!(redis["detail"], "ok");
    let storage = component(&json, "storage");
    assert_eq!(storage["ok"], true, "storage must be ok: {body}");
    assert_eq!(storage["detail"], "ok");
    let models = component(&json, "models");
    assert_eq!(models["ok"], true, "models must be ok: {body}");
    // P0-066: every component reports its probe latency.
    assert_latencies(&json);

    fake.abort();
    stop_api(&mut child).await;
}

#[tokio::test]
async fn readyz_degraded_when_database_down() {
    let api_port = free_port();
    let metrics_port = free_port();
    // Closed port: boot must still succeed (lazy pool) — only
    // /readyz may report the database down.
    let mut child = spawn_api(
        api_port,
        metrics_port,
        Some("postgres://pf:pf@127.0.0.1:59999/pf"),
        None,
        None,
    );
    wait_for_healthz(api_port).await;

    let (status, body) = http_get(api_port, "/readyz").await.expect("GET /readyz");
    assert!(
        status.starts_with("HTTP/1.1 503"),
        "readyz must be 503 with the db down, got {status}: {body}"
    );
    let json: serde_json::Value = serde_json::from_str(&body).expect("readyz body is JSON");
    assert_eq!(json["status"], "degraded");
    let db = component(&json, "db");
    assert_eq!(db["ok"], false);
    assert!(
        db["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("unavailable"),
        "detail must say unavailable: {body}"
    );
    // P0-066: the down component still reports its latency.
    assert_latencies(&json);

    stop_api(&mut child).await;
}

#[tokio::test]
async fn readyz_degraded_when_redis_down() {
    let api_port = free_port();
    let metrics_port = free_port();
    // Closed redis port: the db is up, redis is down — /readyz
    // must be 503 with the redis component down and the db ok.
    let mut child = spawn_api(
        api_port,
        metrics_port,
        None,
        Some("redis://127.0.0.1:59998"),
        None,
    );
    wait_for_healthz(api_port).await;

    let (status, body) = http_get(api_port, "/readyz").await.expect("GET /readyz");
    assert!(
        status.starts_with("HTTP/1.1 503"),
        "readyz must be 503 with redis down, got {status}: {body}"
    );
    let json: serde_json::Value = serde_json::from_str(&body).expect("readyz body is JSON");
    assert_eq!(json["status"], "degraded");
    assert_eq!(component(&json, "db")["ok"], true);
    let redis = component(&json, "redis");
    assert_eq!(redis["ok"], false);
    assert!(
        redis["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("unavailable"),
        "detail must say unavailable: {body}"
    );
    // P0-066: every component reports its latency, including the
    // down one.
    assert_latencies(&json);

    stop_api(&mut child).await;
}

#[tokio::test]
async fn readyz_degraded_when_storage_down() {
    let api_port = free_port();
    let metrics_port = free_port();
    // Closed S3 port: db + redis are up, storage is down — /readyz
    // must be 503 with the storage component down and the rest ok.
    // The models component is ok in Phase 0 (no model required).
    let mut child = spawn_api(
        api_port,
        metrics_port,
        None,
        None,
        Some(&format!("http://127.0.0.1:{}", free_port())),
    );
    wait_for_healthz(api_port).await;

    let (status, body) = http_get(api_port, "/readyz").await.expect("GET /readyz");
    assert!(
        status.starts_with("HTTP/1.1 503"),
        "readyz must be 503 with storage down, got {status}: {body}"
    );
    let json: serde_json::Value = serde_json::from_str(&body).expect("readyz body is JSON");
    assert_eq!(json["status"], "degraded");
    assert_eq!(component(&json, "db")["ok"], true);
    assert_eq!(component(&json, "redis")["ok"], true);
    assert_eq!(component(&json, "models")["ok"], true);
    let storage = component(&json, "storage");
    assert_eq!(storage["ok"], false);
    assert!(
        storage["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("unavailable"),
        "detail must say unavailable: {body}"
    );
    // P0-066: every component reports its latency.
    assert_latencies(&json);

    stop_api(&mut child).await;
}
