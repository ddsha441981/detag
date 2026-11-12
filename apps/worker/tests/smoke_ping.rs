//! P0-082: the smoke job end to end — api up, ping enqueued,
//! worker processes it, completion asserted.
//!
//! Spawns the real api + worker binaries (the shutdown-test
//! pattern) against a unique queue namespace, then runs the
//! `smoke-ping` bin (P0-082) against them and asserts it exits 0
//! with `smoke ping done`. This is the same assertion `just
//! smoke` (P0-090) will make against the compose stack; here it
//! runs against the native stack, which is all that exists
//! before E0.9.
//!
//! Honest boundary (see the bin's docs): the probe proves the
//! redis → worker leg plus the api being up. It does not prove
//! the api enqueues — `POST /v1/jobs` is a 501 stub in Phase 0
//! and job creation lands in Phase 1.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::sleep;
use uuid::Uuid;

/// A free port, found by binding :0 and releasing it.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind :0 for a free port")
        .local_addr()
        .expect("port of :0 listener")
        .port()
}

/// Spawn a thread reading the child's stdout lines into a channel.
fn stdout_lines(child: &mut Child) -> Receiver<String> {
    let stdout = child.stdout.take().expect("worker stdout is piped");
    let (tx, rx) = channel::<String>();
    std::thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            match line {
                Ok(line) => {
                    if tx.send(line).is_err() {
                        return;
                    }
                }
                Err(_) => return,
            }
        }
    });
    rx
}

/// Read channel lines until one contains `needle` (panics after 10 s).
async fn wait_for_log_line(rx: &Receiver<String>, needle: &str, child: &mut Child) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        match rx.try_recv() {
            Ok(line) => {
                if line.contains(needle) {
                    return;
                }
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                panic!("worker stdout closed before: {needle}")
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                if child.try_wait().expect("try_wait").is_some() {
                    panic!("worker exited before logging '{needle}'");
                }
                if std::time::Instant::now() > deadline {
                    panic!("worker did not log '{needle}' within 10 s");
                }
                sleep(Duration::from_millis(50)).await;
            }
        }
    }
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
async fn wait_for_exit(child: &mut Child, what: &str) -> std::process::ExitStatus {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status,
            Ok(None) => {
                if std::time::Instant::now() > deadline {
                    let _ = child.kill();
                    panic!("{what} did not exit within 10 s of SIGTERM");
                }
            }
            Err(err) => panic!("try_wait: {err}"),
        }
        sleep(Duration::from_millis(100)).await;
    }
}

fn sigterm(child: &Child, what: &str) {
    let pid = child.id();
    let rc = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    assert_eq!(rc, 0, "SIGTERM to {what} pid {pid} must succeed");
}

/// The api binary's path. `CARGO_BIN_EXE_detag-api` is not set
/// for this package's tests (it belongs to the api package), so
/// resolve it from the workspace target dir. `cargo test
/// --workspace` (what `just verify` runs) builds it; a lone
/// `cargo test -p detag-worker` needs `cargo build -p detag-api`
/// first — the panic says so plainly instead of failing on spawn.
fn detag_api_bin() -> std::path::PathBuf {
    let target_dir = std::env::var("CARGO_TARGET_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("..")
                .join("target")
        });
    let bin = target_dir.join("debug").join("detag-api");
    assert!(
        bin.exists(),
        "detag-api binary not found at {} — build it first: cargo build -p detag-api",
        bin.display()
    );
    bin
}

fn redis_url() -> String {
    std::env::var("PF_REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".to_string())
}

fn database_url() -> String {
    std::env::var("PF_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://pf:pf@localhost:5432/pf".to_string())
}

#[tokio::test]
async fn smoke_ping_runs_the_api_redis_worker_loop() {
    let namespace = format!("pf:test-smoke-{}", Uuid::now_v7());
    let api_port = free_port();
    let api_metrics_port = free_port();
    let worker_metrics_port = free_port();

    let mut api = Command::new(detag_api_bin())
        .env("PF_API_PORT", api_port.to_string())
        .env("PF_METRICS_PORT", api_metrics_port.to_string())
        .env("PF_DATABASE_URL", database_url())
        .env("PF_REDIS_URL", redis_url())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn detag-api");
    let mut worker = Command::new(env!("CARGO_BIN_EXE_detag-worker"))
        .env("PF_REDIS_URL", redis_url())
        .env("PF_DATABASE_URL", database_url())
        .env("PF_WORKER_QUEUE_NAMESPACE", &namespace)
        .env("PF_WORKER_METRICS_PORT", worker_metrics_port.to_string())
        .env("PF_WORKER_CONCURRENCY", "1")
        .env("PF_WORKER_HEARTBEAT_INTERVAL_SECS", "1")
        .env("PF_WORKER_DRAIN_TIMEOUT_SECS", "10")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn detag-worker");
    let rx = stdout_lines(&mut worker);

    wait_for_healthz(api_port).await;
    wait_for_log_line(&rx, "started (queues:", &mut worker).await;

    let Output {
        status,
        stdout,
        stderr,
    } = Command::new(env!("CARGO_BIN_EXE_smoke-ping"))
        .env("PF_REDIS_URL", redis_url())
        .env("PF_DATABASE_URL", database_url())
        .env("PF_WORKER_QUEUE_NAMESPACE", &namespace)
        .env("PF_WORKER_METRICS_PORT", worker_metrics_port.to_string())
        .env("PF_API_PORT", api_port.to_string())
        .arg("--timeout-secs")
        .arg("20")
        .output()
        .expect("run smoke-ping");
    let stdout = String::from_utf8_lossy(&stdout).into_owned();
    let stderr = String::from_utf8_lossy(&stderr).into_owned();
    assert!(
        status.success(),
        "smoke-ping must exit 0, got {status}\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("smoke ping done"),
        "smoke-ping must report done, got stdout: {stdout}"
    );

    sigterm(&worker, "worker");
    sigterm(&api, "api");
    assert!(
        wait_for_exit(&mut worker, "worker").await.success(),
        "worker must exit 0 on SIGTERM"
    );
    assert!(
        wait_for_exit(&mut api, "api").await.success(),
        "api must exit 0 on SIGTERM"
    );
}
