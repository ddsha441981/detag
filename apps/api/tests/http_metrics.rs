//! P0-028: the spawned api server counts + times requests by route
//! template, and the histogram + in-flight gauge show up in the scrape.
//!
//! Counts are asserted as DELTAS between two scrapes: the /healthz
//! readiness poll already makes requests, so absolute values would be
//! timing-dependent.

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

/// Scrape /metrics from the internal port.
async fn scrape(metrics_port: u16) -> String {
    http_get(metrics_port, "/metrics")
        .await
        .expect("scrape /metrics")
        .1
}

/// Value of `detag_http_requests_total{method,route,status}`, or 0 when
/// the line is absent (a counter with no observations has no line).
fn counter(body: &str, method: &str, route: &str, status: &str) -> u64 {
    let prefix = format!(
        "detag_http_requests_total{{method=\"{method}\",route=\"{route}\",status=\"{status}\"}} "
    );
    for line in body.lines() {
        if let Some(value) = line.strip_prefix(&prefix) {
            return value
                .trim()
                .parse()
                .unwrap_or_else(|_| panic!("counter value must parse: {line}"));
        }
    }
    0
}

#[tokio::test]
async fn http_metrics_count_by_template_and_histogram() {
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

    let before = scrape(metrics_port).await;

    // Three known /healthz hits + one 404.
    for _ in 0..3 {
        let (status, _) = http_get(api_port, "/healthz").await.expect("GET /healthz");
        assert!(status.starts_with("HTTP/1.1 200"), "healthz must be 200");
    }
    let (status, _) = http_get(api_port, "/no-such-route")
        .await
        .expect("GET /no-such-route");
    assert!(
        status.starts_with("HTTP/1.1 404"),
        "unknown route must be 404"
    );

    let after = scrape(metrics_port).await;

    // Counter deltas, by route template.
    assert_eq!(
        counter(&after, "GET", "/healthz", "200") - counter(&before, "GET", "/healthz", "200"),
        3,
        "three /healthz hits must add 3"
    );
    assert_eq!(
        counter(&after, "GET", "unmatched", "404") - counter(&before, "GET", "unmatched", "404"),
        1,
        "one 404 must add 1 under route=unmatched"
    );
    // AC: histograms present with template labels.
    assert!(
        after.contains(
            "detag_http_request_duration_seconds_count{method=\"GET\",route=\"/healthz\",status=\"200\"}"
        ),
        "histogram count missing:\n{after}"
    );
    assert!(
        after.contains(
            "detag_http_request_duration_seconds_bucket{method=\"GET\",route=\"/healthz\",status=\"200\""
        ),
        "histogram buckets missing:\n{after}"
    );
    // In-flight gauge present at rest (0 — nothing in flight now).
    assert!(
        after.contains("detag_http_requests_inflight{method=\"GET\",route=\"/healthz\"} 0"),
        "in-flight gauge missing:\n{after}"
    );

    // Clean shutdown.
    let rc = unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
    assert_eq!(rc, 0, "SIGTERM must succeed");
    let status = wait_for_exit(&mut child).await;
    assert!(
        status.success(),
        "api must exit 0 after SIGTERM, got {status}"
    );
}
