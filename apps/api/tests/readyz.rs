//! P0-035: /readyz reflects db health — end to end, against the
//! spawned api binary.
//!
//! Up case: the binary boots with the default PF_DATABASE_URL (the
//! dev database — native postgres on this VM, compose postgres where
//! docker runs) and /readyz answers 200 with the db component ok.
//! Down case: the binary boots with PF_DATABASE_URL pointed at a
//! closed port — boot still succeeds (lazy pool, P0-035) and /readyz
//! answers 503 with the db component down.

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

fn spawn_api(api_port: u16, metrics_port: u16, database_url: Option<&str>) -> Child {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_detag-api"));
    cmd.env("PF_API_PORT", api_port.to_string())
        .env("PF_METRICS_PORT", metrics_port.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(url) = database_url {
        cmd.env("PF_DATABASE_URL", url);
    }
    cmd.spawn().expect("spawn detag-api")
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
async fn readyz_ok_when_database_up() {
    let api_port = free_port();
    let metrics_port = free_port();
    let mut child = spawn_api(api_port, metrics_port, None);
    wait_for_healthz(api_port).await;

    let (status, body) = http_get(api_port, "/readyz").await.expect("GET /readyz");
    assert!(
        status.starts_with("HTTP/1.1 200"),
        "readyz must be 200 with the db up, got {status}: {body}"
    );
    let json: serde_json::Value = serde_json::from_str(&body).expect("readyz body is JSON");
    assert_eq!(json["status"], "ok");
    assert_eq!(json["components"][0]["name"], "db");
    assert_eq!(json["components"][0]["ok"], true);
    assert_eq!(json["components"][0]["detail"], "ok");

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
    );
    wait_for_healthz(api_port).await;

    let (status, body) = http_get(api_port, "/readyz").await.expect("GET /readyz");
    assert!(
        status.starts_with("HTTP/1.1 503"),
        "readyz must be 503 with the db down, got {status}: {body}"
    );
    let json: serde_json::Value = serde_json::from_str(&body).expect("readyz body is JSON");
    assert_eq!(json["status"], "degraded");
    assert_eq!(json["components"][0]["name"], "db");
    assert_eq!(json["components"][0]["ok"], false);
    assert!(
        json["components"][0]["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("unavailable"),
        "detail must say unavailable: {body}"
    );

    stop_api(&mut child).await;
}
