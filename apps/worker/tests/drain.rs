//! P0-081: graceful drain — SIGTERM finishes the in-flight
//! job (exit 0, no lost job) or, when the drain bound expires,
//! exits non-zero promptly instead of hanging a deploy.
//!
//! Both tests spawn the real worker binary (the shutdown.rs
//! pattern: `std::process`, reader thread + channel for stdout
//! lines) against a unique queue namespace
//! (PF_WORKER_QUEUE_NAMESPACE, P0-081) so test jobs never land
//! in the dev queues. A `ping` with `sleep_ms` (the P0-081
//! drain-drill seam) holds a job in flight while SIGTERM lands.
//!
//! The timeout test asserts the BOUND (fast non-zero exit, job
//! not falsely claimed handled). The job itself is not lost
//! either: an un-acked job re-enters the queue when the
//! visibility timeout expires (NFR-21) — proving that
//! end-to-end is the Phase 3 kill-mid-job drill, not this
//! ticket; this test only proves the drain does not hang and
//! does not lie about the job.

use apalis::prelude::*;
use apalis_redis::RedisStorage;
use detag_worker::envelope::JobEnvelope;
use detag_worker::workers::{apalis_config, WorkerConfig};
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::sleep;
use uuid::Uuid;

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

/// Read channel lines until one contains `needle` (panics after 10 s);
/// returns the matched line.
async fn wait_for_log_line(rx: &Receiver<String>, needle: &str, child: &mut Child) -> String {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        match rx.try_recv() {
            Ok(line) => {
                if line.contains(needle) {
                    return line;
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

/// Drain whatever lines are left in the channel (after the child
/// died and its reader thread ended) and say whether `needle`
/// ever appeared. Used for absence assertions: once the channel
/// disconnects, no more lines can arrive, so absence is final.
fn channel_contains(rx: &Receiver<String>, needle: &str) -> bool {
    loop {
        match rx.try_recv() {
            Ok(line) => {
                if line.contains(needle) {
                    return true;
                }
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => return false,
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

/// Wait for the child to exit (panics after `secs`, killing it first).
async fn wait_for_exit(child: &mut Child, secs: u64) -> std::process::ExitStatus {
    let deadline = std::time::Instant::now() + Duration::from_secs(secs);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status,
            Ok(None) => {
                if std::time::Instant::now() > deadline {
                    let _ = child.kill();
                    panic!("worker did not exit within {secs} s");
                }
            }
            Err(err) => panic!("try_wait: {err}"),
        }
        sleep(Duration::from_millis(100)).await;
    }
}

/// A free loopback port, found by binding :0. The child binds
/// it right after spawn; the race window is milliseconds.
async fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind a free port")
        .local_addr()
        .expect("port of :0 listener")
        .port()
}

/// Minimal HTTP GET over a raw socket; returns (status line, body).
async fn http_get(port: u16, path: &str) -> (String, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("connect to the metrics server");
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").as_bytes(),
        )
        .await
        .expect("write the request");
    let mut raw = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut raw))
        .await
        .expect("response within 5 s")
        .expect("read the response");
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text
        .split_once("\r\n\r\n")
        .unwrap_or_else(|| panic!("no header/body split in response: {text}"));
    (
        head.lines().next().unwrap_or("").to_string(),
        body.to_string(),
    )
}

/// Wait until the scrape shows `detag_worker_inflight_jobs N`.
async fn wait_for_inflight(port: u16, n: u64) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let (status, body) = http_get(port, "/metrics").await;
        assert!(status.starts_with("HTTP/1.1 200"), "scrape 200: {status}");
        if body.contains(&format!("detag_worker_inflight_jobs {n}")) {
            return;
        }
        if std::time::Instant::now() > deadline {
            panic!("in-flight did not reach {n} in 10 s:\n{body}");
        }
        sleep(Duration::from_millis(100)).await;
    }
}

fn redis_url() -> String {
    std::env::var("PF_REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".to_string())
}

fn database_url() -> String {
    std::env::var("PF_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://pf:pf@localhost:5432/pf".to_string())
}

fn spawn_worker(namespace: &str, metrics_port: u16, drain_timeout_secs: u64) -> Child {
    Command::new(env!("CARGO_BIN_EXE_detag-worker"))
        .env("PF_REDIS_URL", redis_url())
        .env("PF_DATABASE_URL", database_url())
        .env("PF_WORKER_QUEUE_NAMESPACE", namespace)
        .env("PF_WORKER_METRICS_PORT", metrics_port.to_string())
        .env(
            "PF_WORKER_DRAIN_TIMEOUT_SECS",
            drain_timeout_secs.to_string(),
        )
        .env("PF_WORKER_CONCURRENCY", "1")
        .env("PF_WORKER_HEARTBEAT_INTERVAL_SECS", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("worker binary spawns")
}

async fn enqueue_sleeping_ping(namespace: &str, sleep_ms: u64) -> Uuid {
    let config = WorkerConfig {
        redis_url: redis_url(),
        concurrency: 1,
        namespace_prefix: namespace.to_string(),
        dlq_prefix: format!("pf:test-dlq-{}", Uuid::now_v7()),
        visibility_timeout: Duration::from_secs(300),
        keep_alive: Duration::from_secs(30),
    };
    let conn = apalis_redis::connect(config.redis_url.as_str())
        .await
        .expect("Redis connects");
    let mut storage = RedisStorage::new_with_config(conn, apalis_config(&config, "pipeline"));
    let job_id = Uuid::now_v7();
    storage
        .push(JobEnvelope::new(
            job_id,
            "ping",
            serde_json::json!({"probe": true, "sleep_ms": sleep_ms}),
        ))
        .await
        .expect("ping enqueues");
    job_id
}

fn sigterm(child: &Child) {
    let pid = child.id();
    let rc = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    assert_eq!(rc, 0, "SIGTERM to pid {pid} must succeed");
}

#[tokio::test]
async fn slow_job_finishes_on_sigterm_and_exits_zero() {
    let namespace = format!("pf:test-drain-{}", Uuid::now_v7());
    let port = free_port().await;
    let mut child = spawn_worker(&namespace, port, 10);
    let rx = stdout_lines(&mut child);
    wait_for_log_line(&rx, "started (queues:", &mut child).await;
    let job_id = enqueue_sleeping_ping(&namespace, 2000).await;
    // The job is inside the handler now — SIGTERM must wait for it.
    wait_for_inflight(port, 1).await;
    sigterm(&child);
    // The handler finishes first (the handled line proves the job
    // completed, not lost), then the process exits 0 — and it
    // handled THIS job, not a stale one (the line carries the id).
    let handled = wait_for_log_line(&rx, "ping handled (stub)", &mut child).await;
    assert!(
        handled.contains(&job_id.to_string()),
        "the handled ping must be job {job_id}, got: {handled}"
    );
    let status = wait_for_exit(&mut child, 20).await;
    assert!(
        status.success(),
        "worker must exit 0 after draining the in-flight job, got {status}"
    );
}

#[tokio::test]
async fn drain_timeout_exits_nonzero_without_losing_the_job() {
    let namespace = format!("pf:test-drain-{}", Uuid::now_v7());
    let port = free_port().await;
    let mut child = spawn_worker(&namespace, port, 2);
    let rx = stdout_lines(&mut child);
    wait_for_log_line(&rx, "started (queues:", &mut child).await;
    let job_id = enqueue_sleeping_ping(&namespace, 30_000).await;
    wait_for_inflight(port, 1).await;
    let sent = std::time::Instant::now();
    sigterm(&child);
    let status = wait_for_exit(&mut child, 15).await;
    let elapsed = sent.elapsed();
    assert!(
        !status.success(),
        "a drain that cannot finish in 2 s must exit non-zero, got {status}"
    );
    assert!(
        elapsed < Duration::from_secs(10),
        "the drain bound must fire promptly (took {elapsed:?}, not the 30 s handler sleep)"
    );
    // The job did NOT complete — and the worker must not claim it
    // did. It stays un-acked and re-enters the queue when the
    // visibility timeout expires (NFR-21; Phase 3 drill proves it).
    assert!(
        !channel_contains(&rx, "ping handled (stub)"),
        "the timed-out job {job_id} must not be reported handled"
    );
}
