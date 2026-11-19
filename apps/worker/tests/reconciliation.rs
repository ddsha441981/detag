//! P1-082: the real `reconciliation` handler, end to end.
//!
//! Spawns the real worker binary against a scratch Postgres
//! DB (testkit `TestCtx`, migrated) and the native Redis,
//! seeds drift (a DB row for yesterday + a corrupted Redis
//! hash for the same day), pushes one `reconciliation`
//! envelope onto the `reconciliation` queue, and asserts
//! the worker corrects the hash to the DB values and writes
//! the `billing.usage.reconciled` audit row — the ticket's
//! AC through the real dispatch path, not just the engine.
//!
//! The drift is seeded for YESTERDAY on the real clock
//! (the worker runs `SystemClock`): the job's window is
//! today + yesterday. No other suite writes usage hashes
//! for real-today dates (usage_live freezes 2026-11-15/16,
//! reconciliation_live freezes March 2027), so the run's
//! Redis SCAN cannot touch another test's hash — and the
//! DB leg only ever sees this test's scratch database.

use apalis::prelude::*;
use apalis_redis::{Config as RedisConfig, RedisStorage};
use chrono::Utc;
use deadpool_redis::redis::cmd;
use detag_core::reconciliation::RECONCILE_AUDIT_ACTION;
use detag_core::redis::keys::usage_key;
use detag_core::redis::{create_pool, RedisPoolConfig};
use detag_core::repos::{audit_logs, usage_counters, users};
use detag_testkit::TestCtx;
use detag_worker::envelope::JobEnvelope;
use detag_worker::workers::WorkerConfig;
use serde_json::json;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use tokio::time::sleep;
use uuid::Uuid;

/// A free loopback port, found by binding :0 and dropping the
/// listener (the same small race the codebase already accepts
/// for "closed" ports — the worker binds it immediately after,
/// before it starts serving metrics).
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind :0 for a free port")
        .local_addr()
        .expect("free port")
        .port()
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
async fn reconciliation_handler_corrects_drift_and_audits() {
    let ctx = TestCtx::new().await;
    let namespace = format!("pf:test-recon-{}", Uuid::now_v7());
    let config = WorkerConfig {
        redis_url: ctx.redis_url.clone(),
        concurrency: 1,
        namespace_prefix: namespace.clone(),
        dlq_prefix: format!("pf:test-dlq-{}", Uuid::now_v7()),
        visibility_timeout: Duration::from_secs(300),
        keep_alive: Duration::from_secs(30),
    };
    let redis =
        create_pool(&ctx.redis_url, &RedisPoolConfig::default()).expect("redis pool builds");
    let user_id = users::create(
        &ctx.pool,
        "recon-worker@example.com",
        "argon2id$placeholder",
    )
    .await
    .expect("create user")
    .id;
    // Yesterday on the real clock: the DB says (5, 120),
    // Redis says (8, 300) — drift past the 60 s threshold,
    // so the run must correct AND audit.
    let yesterday = Utc::now().date_naive() - chrono::Duration::days(1);
    usage_counters::increment(&ctx.pool, user_id, yesterday, 5, 120)
        .await
        .expect("seed the DB row");
    {
        let mut conn = redis.get().await.expect("checkout");
        let _: () = cmd("HSET")
            .arg(usage_key(&user_id, yesterday))
            .arg("images")
            .arg(8)
            .arg("video_seconds")
            .arg(300)
            .query_async(&mut conn)
            .await
            .expect("HSET the drifted hash");
    }

    let mut child = Command::new(env!("CARGO_BIN_EXE_detag-worker"))
        .env("PF_DATABASE_URL", &ctx.database_url)
        .env("PF_REDIS_URL", &ctx.redis_url)
        .env("PF_WORKER_QUEUE_NAMESPACE", &namespace)
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

    // Enqueue one reconciliation job on the reconciliation queue.
    let conn = apalis_redis::connect(config.redis_url.as_str())
        .await
        .expect("Redis connects");
    let mut storage = RedisStorage::new_with_config(
        conn,
        RedisConfig::default().set_namespace(&config.queue_namespace("reconciliation")),
    );
    storage
        .push(JobEnvelope::new(
            Uuid::now_v7(),
            "reconciliation",
            json!({"trigger": "test"}),
        ))
        .await
        .expect("envelope enqueues");

    // Poll the hash until the worker corrects it (15 s).
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let corrected = loop {
        let mut conn = redis.get().await.expect("checkout");
        let got: (Option<i64>, Option<i64>) = cmd("HMGET")
            .arg(usage_key(&user_id, yesterday))
            .arg("images")
            .arg("video_seconds")
            .query_async(&mut conn)
            .await
            .expect("HMGET the day hash");
        if (got.0.unwrap_or(0), got.1.unwrap_or(0)) == (5, 120) {
            break true;
        }
        if tokio::time::Instant::now() > deadline {
            break false;
        }
        sleep(Duration::from_millis(100)).await;
    };
    assert!(
        corrected,
        "the worker did not correct the drifted hash within 15 s"
    );

    // The anomaly audit row landed in the scratch DB.
    let rows = audit_logs::list_for_action(&ctx.pool, RECONCILE_AUDIT_ACTION)
        .await
        .expect("read the audit rows");
    assert_eq!(rows.len(), 1, "exactly one anomaly audit row");
    assert_eq!(
        rows[0].target_id.as_deref(),
        Some(user_id.to_string()).as_deref()
    );
    assert_eq!(rows[0].before.as_ref().unwrap()["video_seconds"], 300);
    assert_eq!(rows[0].after.as_ref().unwrap()["video_seconds"], 120);

    // Tidy the hash, then SIGTERM (what orchestrators send):
    // the worker exits 0.
    {
        let mut conn = redis.get().await.expect("checkout");
        let _: () = cmd("DEL")
            .arg(usage_key(&user_id, yesterday))
            .query_async(&mut conn)
            .await
            .expect("DEL the day hash");
    }
    let pid = child.id();
    let rc = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    assert_eq!(rc, 0, "SIGTERM to pid {pid} must succeed");
    let status = wait_for_exit(&mut child).await;
    assert!(
        status.success(),
        "worker must exit 0 on SIGTERM, got {status}"
    );

    ctx.close().await;
}
