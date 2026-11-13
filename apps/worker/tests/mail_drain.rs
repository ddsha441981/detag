//! P1-028: the spawned worker drains the outbox over SMTP.
//!
//! Spawns the real `detag-worker` binary against a scratch
//! Postgres (testkit `TestCtx`) and a fake SMTP server on a
//! loopback ephemeral port (the same minimal protocol subset
//! as the mailer's own fake relay: greeting, EHLO, MAIL FROM,
//! RCPT TO, DATA, QUIT). One `verify-email` row is enqueued
//! before boot; the drain interval is 1 s, so the mail must
//! arrive within a few seconds of the `started` log line —
//! and the row must be marked sent, so a second pass sends
//! nothing. SIGTERM then exits 0, proving the drain loop
//! stops with the process.
//!
//! The child is spawned with `std::process` (the api/worker
//! shutdown tests' pattern) — the workspace tokio has no
//! `process` feature. A reader thread feeds stdout lines into
//! a channel the async test polls.

use detag_core::checksum::sha256_hex;
use detag_core::repos::{email_outbox, email_outbox::NewOutboxMail, users};
use detag_testkit::TestCtx;
use serde_json::json;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader as TokioBufReader};
use tokio::net::TcpListener;
use tokio::time::sleep;

/// A free loopback port, found by binding :0 and dropping the
/// listener (the same small race the codebase already accepts
/// for "closed" ports — the fake relay binds it immediately
/// after, before the child starts).
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind :0 for a free port")
        .local_addr()
        .expect("free port")
        .port()
}

/// Minimal fake SMTP server: answers the greeting, EHLO, MAIL
/// FROM, RCPT TO, DATA (captures the message) and QUIT — just
/// enough SMTP to prove the worker's lettre transport really
/// speaks the protocol. `capture` receives the raw DATA bytes
/// (headers + body) of the first mail, then the server exits.
async fn run_fake_smtp(listener: TcpListener, capture: Arc<Mutex<Option<String>>>) {
    let (mut stream, _) = listener.accept().await.expect("fake smtp accept");
    let (reader, mut writer) = stream.split();
    let mut lines = TokioBufReader::new(reader).lines();
    writer
        .write_all(b"220 localhost fake-smtp\r\n")
        .await
        .expect("fake smtp greeting");
    while let Some(line) = lines.next_line().await.expect("fake smtp read") {
        let upper = line.to_uppercase();
        if upper.starts_with("EHLO") || upper.starts_with("HELO") {
            writer
                .write_all(b"250-localhost\r\n250 SMTPUTF8\r\n")
                .await
                .expect("fake smtp EHLO");
        } else if upper.starts_with("MAIL FROM") {
            writer
                .write_all(b"250 OK\r\n")
                .await
                .expect("fake smtp MAIL");
        } else if upper.starts_with("RCPT TO") {
            writer
                .write_all(b"250 OK\r\n")
                .await
                .expect("fake smtp RCPT");
        } else if upper.starts_with("DATA") {
            writer
                .write_all(b"354 End data with <CR><LF>.<CR><LF>\r\n")
                .await
                .expect("fake smtp DATA");
            let mut data = String::new();
            while let Some(data_line) = lines.next_line().await.expect("fake smtp data") {
                if data_line == "." {
                    break;
                }
                data.push_str(&data_line);
                data.push('\n');
            }
            *capture.lock().expect("capture lock") = Some(data);
            writer
                .write_all(b"250 OK\r\n")
                .await
                .expect("fake smtp queued");
        } else if upper.starts_with("QUIT") {
            writer
                .write_all(b"221 Bye\r\n")
                .await
                .expect("fake smtp QUIT");
            break;
        } else if upper.starts_with("RSET") {
            writer
                .write_all(b"250 OK\r\n")
                .await
                .expect("fake smtp RSET");
        } else {
            writer
                .write_all(b"500 unrecognized\r\n")
                .await
                .expect("fake smtp unknown");
        }
    }
}

/// Spawn a thread reading the child's stdout lines into a channel.
fn stdout_lines(child: &mut Child) -> std::sync::mpsc::Receiver<String> {
    let stdout = child.stdout.take().expect("worker stdout is piped");
    let (tx, rx) = std::sync::mpsc::channel::<String>();
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
async fn wait_for_log_line(
    rx: &std::sync::mpsc::Receiver<String>,
    needle: &str,
    child: &mut Child,
) {
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
async fn worker_drains_the_outbox_over_smtp_and_exits_cleanly() {
    let ctx = TestCtx::new().await;
    let user_id = users::create(&ctx.pool, "ivy@example.com", "argon2id$placeholder")
        .await
        .expect("create user")
        .id;
    let row_id = email_outbox::enqueue(
        &ctx.pool,
        &NewOutboxMail {
            to_hash: sha256_hex("ivy@example.com".as_bytes()),
            template: "verify-email".to_string(),
            payload: json!({ "user_id": user_id, "token": "tok-worker-1" }),
            scheduled_at: None,
        },
    )
    .await
    .expect("enqueue outbox row");

    // The fake relay, bound before the child starts so its port
    // is fixed in the child's environment.
    let smtp_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("fake smtp binds");
    let smtp_port = smtp_listener.local_addr().expect("fake smtp port").port();
    let capture: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let server = tokio::spawn(run_fake_smtp(smtp_listener, Arc::clone(&capture)));

    let mut child = Command::new(env!("CARGO_BIN_EXE_detag-worker"))
        .env("PF_DATABASE_URL", &ctx.database_url)
        .env("PF_REDIS_URL", &ctx.redis_url)
        .env("PF_SMTP_HOST", "127.0.0.1")
        .env("PF_SMTP_PORT", smtp_port.to_string())
        .env("PF_SMTP_TLS", "false")
        .env("PF_MAIL_FROM", "noreply@detag.local")
        .env("PF_MAILER_DRAIN_INTERVAL_SECS", "1")
        .env("PF_WORKER_METRICS_PORT", free_port().to_string())
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

    // The drain runs one pass at startup, then every 1 s: the
    // mail must arrive well inside 15 s.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let data = loop {
        if let Some(data) = capture.lock().expect("capture lock").clone() {
            break data;
        }
        if tokio::time::Instant::now() > deadline {
            let _ = child.kill();
            panic!("the worker did not deliver the queued mail within 15 s");
        }
        sleep(Duration::from_millis(100)).await;
    };
    assert!(
        data.contains("To: ivy@example.com"),
        "DATA must carry the recipient:\n{data}"
    );
    assert!(
        data.contains("Subject: Verify your Detag email"),
        "DATA must carry the subject:\n{data}"
    );
    assert!(
        data.contains("tok-worker-1"),
        "DATA must carry the token:\n{data}"
    );
    // Do NOT await the fake relay yet: lettre pools the SMTP
    // connection, so the relay's task only ends when the worker
    // exits and the connection closes. The row check + SIGTERM
    // come first; the relay is awaited after the child exits.
    let remaining = email_outbox::unsent_for_template(&ctx.pool, "verify-email")
        .await
        .expect("read back")
        .into_iter()
        .any(|r| r.id == row_id);
    assert!(!remaining, "the sent row must not be unsent anymore");

    // SIGTERM (what orchestrators send): the drain loop stops
    // with the process, exit 0.
    let pid = child.id();
    let rc = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    assert_eq!(rc, 0, "SIGTERM to pid {pid} must succeed");
    let status = wait_for_exit(&mut child).await;
    assert!(
        status.success(),
        "worker must exit 0 on SIGTERM, got {status}"
    );
    // The child is gone, so its pooled SMTP connection closed
    // and the fake relay's task must finish promptly.
    tokio::time::timeout(Duration::from_secs(10), server)
        .await
        .expect("fake smtp task finishes after the worker exits")
        .expect("fake smtp task joins");

    ctx.close().await;
}
