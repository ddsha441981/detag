//! P0-023: the spawned api server drains an in-flight request on
//! SIGTERM and exits 0.
//!
//! The test holds a request open with a raw socket — a COMPLETE head
//! and a PARTIAL body — sends SIGTERM, completes the body, and
//! asserts the server still answers it — a killed server would drop
//! it, a graceful one drains it. SIGINT shares the same code path
//! (`shutdown_signal` selects on both), so only SIGTERM is exercised
//! here: it is the signal orchestrators send.
//!
//! Why a split BODY, not split headers (FLAKY-001, fixed 2026-11-14):
//! the old version sent partial headers (no final blank line), then
//! SIGTERM, then the blank line — and failed intermittently with
//! `Connection reset by peer`. Root cause, confirmed in the hyper
//! 1.12 source: graceful shutdown is `disable_keep_alive()`, which
//! CLOSES the connection when `is_idle()` — and a connection whose
//! request head is not yet fully parsed counts as idle (the
//! keep-alive state only goes busy once a head is parsed). So a
//! partial-headers request is dropped, not drained, whenever the
//! drain starts before the final blank line arrives. With a complete
//! head + partial body the request IS in flight (the handler awaits
//! the body), so hyper waits for it — and the handler cannot answer
//! before the signal, because it awaits the full body. Deterministic.
//!
//! The body is `{not json` (malformed JSON) to POST /v1/auth/register:
//! the Json extractor rejects it with 422 PF-2002 before any DB
//! write, so the test has no side effects. A 500 ms settle delay
//! after the head lets the server parse it before the signal;
//! without it SIGTERM could land before the parse and hit the same
//! idle-close path.
//!
//! P0-090 regression: `tokio::time::timeout(SHUTDOWN_TIMEOUT, ...)`
//! used to wrap the whole server run, so the api force-exited 30 s
//! after startup even with no signal (the smoke proof caught it:
//! both binaries died mid-smoke). The timeout now bounds only the
//! post-signal drain — `stays_up_past_shutdown_timeout_without_signal`
//! below proves it. It sleeps past the timeout on purpose (~32 s);
//! that is the price of a direct test for a bug that would
//! restart-loop the api in production.

use detag_core::shutdown::SHUTDOWN_TIMEOUT;
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::{sleep, timeout};

/// A free port, found by binding :0 and releasing it. The api binds
/// it immediately after, so the race window is negligible. Each test
/// takes its own api AND metrics ports so the tests stay
/// parallel-safe (two apis on the default metrics port would fail
/// to bind — caught when the second test in this file landed, P0-090).
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
    let metrics_port = free_port();
    let mut child = Command::new(env!("CARGO_BIN_EXE_detag-api"))
        .env("PF_API_PORT", port.to_string())
        .env("PF_METRICS_PORT", metrics_port.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn detag-api");
    wait_for_healthz(port).await;

    // Hold a request open with a COMPLETE head and a PARTIAL body:
    // POST /v1/auth/register, Content-Length 9, body `{not json`
    // (malformed JSON -> 422 PF-2002, no DB writes). The head is
    // parsed at once, so hyper dispatches the request and the Json
    // extractor awaits the rest of the body — the request is
    // genuinely in flight. `Connection: close` so the server closes
    // after answering, which lets the graceful shutdown finish once
    // we are done.
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("connect to api");
    stream
        .write_all(b"POST /v1/auth/register HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\nContent-Length: 9\r\nConnection: close\r\n\r\n{not ")
        .await
        .expect("send request head + partial body");

    // Let the server parse the head and start awaiting the body
    // (~1 ms of work; 500 ms is a generous settle margin). Without
    // this, SIGTERM could land before the head is parsed — hyper
    // treats a connection with no parsed head as idle and closes it
    // on drain (FLAKY-001).
    sleep(Duration::from_millis(500)).await;

    // SIGTERM while the request is in flight.
    let pid = child.id();
    let rc = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    assert_eq!(rc, 0, "SIGTERM to pid {pid} must succeed");

    // Complete the body AFTER the signal: a graceful server still
    // answers it (no dropped in-flight request). The handler cannot
    // have answered before the signal — it awaits the full body —
    // so the response is necessarily produced during the drain.
    stream
        .write_all(b"json")
        .await
        .expect("complete the in-flight request body");
    let mut response = Vec::new();
    timeout(Duration::from_secs(5), stream.read_to_end(&mut response))
        .await
        .expect("response within 5 s")
        .expect("read response");
    let response = String::from_utf8_lossy(&response);
    assert!(
        response.starts_with("HTTP/1.1 422"),
        "in-flight request must get 422, got: {response}"
    );
    assert!(
        response.contains("PF-2002"),
        "in-flight request body must carry PF-2002, got: {response}"
    );
    drop(stream);

    // AC: exit 0 once the last connection drains.
    let status = wait_for_exit(&mut child).await;
    assert!(
        status.success(),
        "api must exit 0 after SIGTERM, got {status}"
    );
}

/// P0-090: with NO signal the api must stay up past
/// SHUTDOWN_TIMEOUT — the timeout bounds only the post-signal
/// drain. (Before the fix this failed: the api force-exited
/// 30 s after startup.) The graceful path must still work
/// afterwards: SIGTERM then exits 0 promptly.
#[tokio::test]
async fn stays_up_past_shutdown_timeout_without_signal() {
    let port = free_port();
    let metrics_port = free_port();
    let mut child = Command::new(env!("CARGO_BIN_EXE_detag-api"))
        .env("PF_API_PORT", port.to_string())
        .env("PF_METRICS_PORT", metrics_port.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn detag-api");
    wait_for_healthz(port).await;

    // Past the timeout, with margin — the server must still answer.
    sleep(SHUTDOWN_TIMEOUT + Duration::from_secs(2)).await;
    wait_for_healthz(port).await;

    // And the graceful path still works after the fix.
    let pid = child.id();
    let rc = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    assert_eq!(rc, 0, "SIGTERM to pid {pid} must succeed");
    let status = wait_for_exit(&mut child).await;
    assert!(
        status.success(),
        "api must exit 0 after SIGTERM, got {status}"
    );
}
