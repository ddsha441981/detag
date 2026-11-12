//! P0-075: the spawned worker starts and exits 0 on SIGTERM.
//!
//! The drain semantics — no lost job — land in P0-081; this test
//! proves the process boots, registers its workers against the
//! native Redis, and shuts down cleanly on the signal
//! orchestrators send. Startup is proven by the `started
//! (queues: ...)` log line on the child's stdout.
//!
//! The child is spawned with `std::process` (the api shutdown
//! test's pattern) — the workspace tokio has no `process`
//! feature, so no `tokio::process` here. A reader thread feeds
//! stdout lines into a channel the async test polls.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;
use tokio::time::sleep;

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

/// Wait for the child to exit (panics after 10 s, killing it first).
async fn wait_for_exit(child: &mut Child) -> std::process::ExitStatus {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status,
            Ok(None) => {
                if std::time::Instant::now() > deadline {
                    let _ = child.kill();
                    panic!("worker did not exit within 10 s of SIGTERM");
                }
            }
            Err(err) => panic!("try_wait: {err}"),
        }
        sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn worker_starts_and_exits_zero_on_sigterm() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_detag-worker"))
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("worker binary spawns");
    let rx = stdout_lines(&mut child);
    wait_for_log_line(&rx, "started (queues:", &mut child).await;
    assert!(
        child.try_wait().expect("try_wait").is_none(),
        "worker must still be running after startup"
    );
    // SIGTERM (what orchestrators send); SIGINT shares the code path.
    let pid = child.id();
    let rc = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    assert_eq!(rc, 0, "SIGTERM to pid {pid} must succeed");
    let status = wait_for_exit(&mut child).await;
    assert!(
        status.success(),
        "worker must exit 0 on SIGTERM, got {status}"
    );
}
