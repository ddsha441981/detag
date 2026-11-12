//! P0-026: the spawned api server scrapes /metrics on its internal
//! port, and /metrics is NOT on the public api port (FR-X-02).
//!
//! Spawns the real binary on free ports, scrapes both ports with a raw
//! socket, and asserts the scrape works and the edge port 404s.

use std::process::{Child, Command, Stdio};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
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
/// `Err` on connect/send/read failure (the healthz poll retries those).
async fn http_get(port: u16, path: &str) -> std::io::Result<(String, String)> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await?;
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").as_bytes(),
        )
        .await?;
    let mut raw = Vec::new();
    timeout(Duration::from_secs(5), stream.read_to_end(&mut raw))
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

#[tokio::test]
async fn metrics_scrape_works_and_is_not_on_the_edge_port() {
    let api_port = free_port();
    let metrics_port = free_port();
    let mut child = Command::new(env!("CARGO_BIN_EXE_detag-api"))
        .env("PF_API_PORT", api_port.to_string())
        .env("PF_METRICS_PORT", metrics_port.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn detag-api");
    wait_for_healthz(api_port).await;

    // AC: metrics scrape works — process metrics + custom build_info.
    let (status, body) = http_get(metrics_port, "/metrics")
        .await
        .expect("GET /metrics on the internal port");
    assert!(
        status.starts_with("HTTP/1.1 200"),
        "GET /metrics on the internal port must be 200, got {status}"
    );
    assert!(
        body.contains("process_cpu_seconds_total"),
        "scrape must include process metrics:\n{body}"
    );
    assert!(
        body.contains("detag_build_info{version=\"0.1.0\"} 1"),
        "scrape must include the custom build_info gauge:\n{body}"
    );

    // AC: not on the edge network — /metrics is not on the api port.
    let (status, _) = http_get(api_port, "/metrics")
        .await
        .expect("GET /metrics on the api port");
    assert!(
        status.starts_with("HTTP/1.1 404"),
        "GET /metrics on the api port must be 404, got {status}"
    );

    // Clean shutdown (the dual-server main must still exit 0).
    let rc = unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
    assert_eq!(rc, 0, "SIGTERM must succeed");
    let status = wait_for_exit(&mut child).await;
    assert!(
        status.success(),
        "api must exit 0 after SIGTERM, got {status}"
    );
}
