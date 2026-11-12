//! P0-080: the heartbeat row and the /metrics gauges are visible.
//!
//! Starts a monitor, the heartbeat loop (1 s interval) and the
//! worker's /metrics server against a scratch database (the
//! worker_heartbeats migration pair, copied from the repo — the
//! same pattern as the migration live tests), then asserts: the
//! heartbeat row exists with this process's pid, the four
//! queues, in-flight 0 and a recent started_at; the scrape
//! shows `detag_worker_info`, `detag_worker_inflight_jobs 0`,
//! a non-zero last heartbeat and at least 2 beats; a pushed
//! `ping` is processed alongside (the monitor and the heartbeat
//! coexist); on shutdown the row is deleted — a graceful stop
//! leaves no stale worker behind.

use apalis::prelude::*;
use apalis_redis::RedisStorage;
use detag_core::db::{connect, DbPool, PoolConfig};
use detag_core::metrics::Metrics;
use detag_core::migrate::{migrate_revert_last, migrate_up};
use detag_core::repos::worker_heartbeats::list_heartbeats;
use detag_worker::envelope::JobEnvelope;
use detag_worker::heartbeat::{run_heartbeat, WorkerIdentity};
use detag_worker::metrics_server::serve_metrics;
use detag_worker::queue_hooks::QueueHooks;
use detag_worker::workers::{apalis_config, build_monitor, WorkerConfig};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;
use uuid::Uuid;

const TEST_DB: &str = "detag_p080_worker";
const STEM: &str = "20261112133104_worker_heartbeats";

fn database_url() -> String {
    std::env::var("PF_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://pf:pf@localhost:5432/pf".to_string())
}

fn test_database_url() -> String {
    let mut url = url::Url::parse(&database_url()).expect("PF_DATABASE_URL parses");
    url.set_path(&format!("/{TEST_DB}"));
    url.to_string()
}

fn test_config() -> WorkerConfig {
    WorkerConfig {
        redis_url: std::env::var("PF_REDIS_URL")
            .unwrap_or_else(|_| "redis://localhost:6379".to_string()),
        concurrency: 1,
        namespace_prefix: format!("pf:test-heartbeat-{}", Uuid::now_v7()),
        dlq_prefix: format!("pf:test-dlq-{}", Uuid::now_v7()),
        visibility_timeout: Duration::from_secs(300),
        keep_alive: Duration::from_secs(30),
    }
}

async fn scratch_pool() -> (DbPool, PathBuf) {
    let admin = connect(&database_url(), &PoolConfig::default())
        .await
        .expect("connect to dev db");
    sqlx::query("DROP DATABASE IF EXISTS \"detag_p080_worker\"")
        .execute(&admin)
        .await
        .expect("drop stale scratch db");
    sqlx::query("CREATE DATABASE \"detag_p080_worker\"")
        .execute(&admin)
        .await
        .expect("create scratch db");
    admin.close().await;
    let pool = connect(&test_database_url(), &PoolConfig::default())
        .await
        .expect("connect to scratch db");
    let dir = std::env::temp_dir().join(format!(
        "detag-p080-worker-migrations-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch migrations dir");
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("migrations");
    for ext in ["up.sql", "down.sql"] {
        let name = format!("{STEM}.{ext}");
        std::fs::copy(repo.join(&name), dir.join(&name)).expect("copy migration pair");
    }
    migrate_up(&pool, &dir).await.expect("migrate up");
    (pool, dir)
}

async fn drop_scratch_db(pool: &DbPool, dir: &PathBuf) {
    let _ = migrate_revert_last(pool, dir).await;
    pool.close().await;
    let admin = connect(&database_url(), &PoolConfig::default())
        .await
        .expect("connect to dev db");
    sqlx::query("DROP DATABASE IF EXISTS \"detag_p080_worker\"")
        .execute(&admin)
        .await
        .expect("drop scratch db");
    admin.close().await;
    let _ = std::fs::remove_dir_all(dir);
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

#[tokio::test]
async fn heartbeat_row_and_gauges_are_visible() {
    let (pool, dir) = scratch_pool().await;
    let config = test_config();
    let metrics = Metrics::new();
    let hooks = QueueHooks::new(metrics.queue().clone());
    hooks.reset_depths();
    let identity = WorkerIdentity::current();
    metrics.worker().set_info(&identity.worker_id, identity.pid);
    let (monitor, inflight) = build_monitor(&config, hooks, &pool)
        .await
        .expect("monitor builds against the native Redis");
    let shutdown = Arc::new(Notify::new());
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind a free metrics port");
    let metrics_port = listener.local_addr().expect("port of :0 listener").port();
    let metrics_task = tokio::spawn(serve_metrics(
        metrics.clone(),
        listener,
        Arc::clone(&shutdown),
    ));
    let heartbeat_task = tokio::spawn(run_heartbeat(
        pool.clone(),
        identity.clone(),
        inflight,
        metrics.worker().clone(),
        Duration::from_secs(1),
        Arc::clone(&shutdown),
    ));
    let (monitor_shutdown_tx, monitor_shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let monitor_task = tokio::spawn(monitor.run_with_signal(async {
        monitor_shutdown_rx.await.ok();
        Ok::<(), std::io::Error>(())
    }));

    // Wait for the second beat: the row exists and the counter
    // reads 2, so the loop is ticking, not a one-off write.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let scrape = loop {
        let (status, body) = http_get(metrics_port, "/metrics").await;
        assert!(status.starts_with("HTTP/1.1 200"), "scrape 200: {status}");
        if body.contains("detag_worker_heartbeats_total 2") {
            break body;
        }
        if tokio::time::Instant::now() > deadline {
            panic!("the heartbeat loop did not beat twice in 10 s:\n{body}");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let rows = list_heartbeats(&pool).await.expect("list reads");
    assert_eq!(rows.len(), 1, "one row for this process");
    let row = &rows[0];
    assert_eq!(row.worker_id, identity.worker_id);
    assert_eq!(row.pid, std::process::id() as i32);
    assert_eq!(
        row.queues,
        ["pipeline", "cleanup", "email", "reconciliation"],
        "the process serves all four queues"
    );
    assert_eq!(row.in_flight, 0, "nothing in flight yet");
    assert!(
        (chrono::Utc::now() - row.started_at).num_seconds().abs() <= 60,
        "started_at is this process's start: {:?}",
        row.started_at
    );
    // The gauges: identity, in-flight 0, a real last heartbeat.
    let info_line = format!(
        "detag_worker_info{{pid=\"{}\",worker_id=\"{}\"}} 1",
        identity.pid, identity.worker_id
    );
    assert!(
        scrape.contains(&info_line),
        "worker info missing:\n{scrape}"
    );
    assert!(
        scrape.contains("detag_worker_inflight_jobs 0"),
        "inflight gauge missing:\n{scrape}"
    );
    assert!(
        scrape.contains("detag_worker_last_heartbeat_seconds "),
        "last heartbeat missing:\n{scrape}"
    );
    assert!(
        !scrape.contains("detag_worker_last_heartbeat_seconds 0"),
        "last heartbeat must be a real timestamp:\n{scrape}"
    );

    // The monitor still works alongside the heartbeat: a pushed
    // ping is processed.
    let conn = apalis_redis::connect(config.redis_url.as_str())
        .await
        .expect("Redis connects");
    let mut storage = RedisStorage::new_with_config(conn, apalis_config(&config, "pipeline"));
    storage
        .push(JobEnvelope::new(
            Uuid::now_v7(),
            "ping",
            serde_json::json!({"probe": true}),
        ))
        .await
        .expect("ping enqueues");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if metrics
            .gather()
            .contains("detag_queue_dequeued_total{queue=\"pipeline\"} 1")
        {
            break;
        }
        if tokio::time::Instant::now() > deadline {
            panic!("the ping was not processed:\n{}", metrics.gather());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // Shutdown: the monitor stops, the heartbeat loop deletes
    // its row, the metrics server stops. A graceful stop leaves
    // no stale worker behind.
    monitor_shutdown_tx.send(()).expect("monitor still running");
    shutdown.notify_waiters();
    let exited = tokio::time::timeout(Duration::from_secs(10), monitor_task)
        .await
        .expect("monitor shuts down within 10 s");
    exited
        .expect("monitor task joins")
        .expect("monitor exits cleanly");
    tokio::time::timeout(Duration::from_secs(10), heartbeat_task)
        .await
        .expect("heartbeat task stops within 10 s")
        .expect("heartbeat task joins");
    tokio::time::timeout(Duration::from_secs(10), metrics_task)
        .await
        .expect("metrics task stops within 10 s")
        .expect("metrics task joins")
        .expect("metrics server exits cleanly");
    let rows = list_heartbeats(&pool).await.expect("list reads again");
    assert!(
        rows.is_empty(),
        "the heartbeat row is deleted on graceful shutdown"
    );
    drop_scratch_db(&pool, &dir).await;
}
