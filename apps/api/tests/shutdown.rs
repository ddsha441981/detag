//! P0-023: the spawned api server drains an in-flight request on
//! SIGTERM and exits 0.
//!
//! The test holds a request open with a raw socket (partial headers,
//! no final blank line), sends SIGTERM, completes the request, and
//! asserts the server still answers it — a killed server would drop
//! it, a graceful one drains it. SIGINT shares the same code path
//! (`shutdown_signal` selects on both), so only SIGTERM is exercised
//! here: it is the signal orchestrators send.

use std::process::{Child, Command, Stdio};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::{sleep, timeout};

/// A free port, found by binding :0 and releasing it. The api binds
/// it immediately after, so the race window is negligible.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind :0 for a free port")
        .local_addr()
        .expect("port of :0 listener")
        .port()
}

/// Poll /healthz until it answers 200 (panics after 10 s).
async fn wait_for_healthz(port: u16) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)).await {
            let wrote = stream
                .write_all(b"GET /healthz HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
                .await
                .is_ok();
            if wrote {
                let mut buf = [0u8; 64];
                if let Ok(n) = stream.read(&mut buf).await {
                    if String::from_utf8_lossy(&buf[..n]).starts_with("HTTP/1.1 200") {
                        return;
                    }
                }
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
async fn sigterm_drains_in_flight_request_and_exits_zero() {
    let port = free_port();
    let mut child = Command::new(env!("CARGO_BIN_EXE_detag-api"))
        .env("PF_API_PORT", port.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn detag-api");
    wait_for_healthz(port).await;

    // Hold a request open: headers without the final blank line, so the
    // server is waiting on this connection — the request is in flight.
    // `Connection: close` so the server closes after answering, which
    // lets the graceful shutdown finish once we are done.
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("connect to api");
    stream
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: x\r\nConnection: close\r\n")
        .await
        .expect("send partial request");

    // SIGTERM while the request is in flight.
    let pid = child.id();
    let rc = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    assert_eq!(rc, 0, "SIGTERM to pid {pid} must succeed");

    // Complete the request AFTER the signal: a graceful server still
    // answers it (no dropped in-flight request).
    stream
        .write_all(b"\r\n")
        .await
        .expect("complete the in-flight request");
    let mut response = Vec::new();
    timeout(Duration::from_secs(5), stream.read_to_end(&mut response))
        .await
        .expect("response within 5 s")
        .expect("read response");
    let response = String::from_utf8_lossy(&response);
    assert!(
        response.starts_with("HTTP/1.1 200"),
        "in-flight request must get 200, got: {response}"
    );
    assert!(
        response.ends_with("ok"),
        "in-flight request body must be 'ok', got: {response}"
    );
    drop(stream);

    // AC: exit 0 once the last connection drains.
    let status = wait_for_exit(&mut child).await;
    assert!(
        status.success(),
        "api must exit 0 after SIGTERM, got {status}"
    );
}
