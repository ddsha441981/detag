//! Live test for the worker heartbeat writer (P0-080, E0.8).
//!
//! Applies ONLY the worker_heartbeats migration pair (copied from
//! the repo's real `migrations/` into a scratch dir — the same
//! pattern as the migration live tests) to a scratch DATABASE
//! (`detag_p080_heartbeat`, created and dropped by the test) —
//! never the dev database. Asserts the upsert keeps one row per
//! worker (started_at frozen, last_heartbeat_at moving), the
//! prune drops only stale rows, and the graceful-shutdown delete
//! removes the row.

use chrono::Utc;
use detag_core::db::{connect, DbPool, PoolConfig};
use detag_core::migrate::{migrate_revert_last, migrate_up};
use detag_core::repos::worker_heartbeats::{
    delete_heartbeat, list_heartbeats, prune_stale_heartbeats, record_heartbeat, HeartbeatRow,
    HEARTBEAT_PRUNE_AFTER,
};
use std::path::PathBuf;

const TEST_DB_UPSERT: &str = "detag_p080_upsert";
const TEST_DB_PRUNE: &str = "detag_p080_prune";
const STEM: &str = "20261112133104_worker_heartbeats";

fn database_url() -> String {
    std::env::var("PF_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://pf:pf@localhost:5432/pf".to_string())
}

fn test_database_url(db: &str) -> String {
    let mut url = url::Url::parse(&database_url()).expect("PF_DATABASE_URL parses");
    url.set_path(&format!("/{db}"));
    url.to_string()
}

fn migrations_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "detag-p080-migrations-{}-{tag}",
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
    dir
}

async fn scratch_pool(db: &str, tag: &str) -> (DbPool, PathBuf) {
    // The names are static literals (the sqlx 0.9 dynamic-SQL lint
    // forbids format! here) — one literal per test db, because the
    // tests in this file run in parallel and cannot share a db.
    let admin = connect(&database_url(), &PoolConfig::default())
        .await
        .expect("connect to dev db");
    let (drop_sql, create_sql): (&str, &str) = match db {
        TEST_DB_UPSERT => (
            "DROP DATABASE IF EXISTS \"detag_p080_upsert\"",
            "CREATE DATABASE \"detag_p080_upsert\"",
        ),
        TEST_DB_PRUNE => (
            "DROP DATABASE IF EXISTS \"detag_p080_prune\"",
            "CREATE DATABASE \"detag_p080_prune\"",
        ),
        _ => panic!("unknown scratch db {db}"),
    };
    sqlx::query(drop_sql)
        .execute(&admin)
        .await
        .expect("drop stale scratch db");
    sqlx::query(create_sql)
        .execute(&admin)
        .await
        .expect("create scratch db");
    admin.close().await;
    let pool = connect(&test_database_url(db), &PoolConfig::default())
        .await
        .expect("connect to scratch db");
    let dir = migrations_dir(tag);
    migrate_up(&pool, &dir).await.expect("migrate up");
    (pool, dir)
}

async fn drop_scratch_db(pool: &DbPool, db: &str, dir: &PathBuf) {
    let _ = migrate_revert_last(pool, dir).await;
    pool.close().await;
    let admin = connect(&database_url(), &PoolConfig::default())
        .await
        .expect("connect to dev db");
    let drop_sql: &str = match db {
        TEST_DB_UPSERT => "DROP DATABASE IF EXISTS \"detag_p080_upsert\"",
        TEST_DB_PRUNE => "DROP DATABASE IF EXISTS \"detag_p080_prune\"",
        _ => panic!("unknown scratch db {db}"),
    };
    sqlx::query(drop_sql)
        .execute(&admin)
        .await
        .expect("drop scratch db");
    admin.close().await;
    let _ = std::fs::remove_dir_all(dir);
}

fn row(worker_id: &str, pid: i32, in_flight: i32) -> HeartbeatRow {
    HeartbeatRow {
        worker_id: worker_id.to_string(),
        pid,
        queues: vec![
            "pipeline".to_string(),
            "cleanup".to_string(),
            "email".to_string(),
            "reconciliation".to_string(),
        ],
        in_flight,
        started_at: Utc::now(),
        last_heartbeat_at: Utc::now(),
    }
}

#[tokio::test]
async fn heartbeat_upsert_keeps_one_row_per_worker() {
    let (pool, dir) = scratch_pool(TEST_DB_UPSERT, "upsert").await;
    let started = Utc::now();
    let first = row("host-1:100", 100, 0);
    record_heartbeat(&pool, &first)
        .await
        .expect("first beat writes");
    let rows = list_heartbeats(&pool).await.expect("list reads");
    assert_eq!(rows.len(), 1, "one row after the first beat");
    assert_eq!(rows[0].worker_id, "host-1:100");
    assert_eq!(rows[0].pid, 100);
    assert_eq!(rows[0].queues.len(), 4);
    assert_eq!(rows[0].in_flight, 0);
    assert!(
        (rows[0].started_at - started).num_seconds().abs() <= 2,
        "started_at is the process start: {:?}",
        rows[0].started_at
    );
    let first_beat_at = rows[0].last_heartbeat_at;
    let first_started_at = rows[0].started_at;

    // A second beat with work in flight updates in place.
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let mut second = row("host-1:100", 100, 2);
    second.started_at = started; // the process start does not move
    record_heartbeat(&pool, &second)
        .await
        .expect("second beat writes");
    let rows = list_heartbeats(&pool).await.expect("list reads again");
    assert_eq!(rows.len(), 1, "still one row — the upsert updated");
    assert_eq!(rows[0].in_flight, 2, "in-flight count overwrote");
    assert_eq!(
        rows[0].started_at, first_started_at,
        "started_at frozen across beats"
    );
    assert!(
        (rows[0].started_at - started).num_seconds().abs() <= 2,
        "started_at still the process start: {:?}",
        rows[0].started_at
    );
    assert!(
        rows[0].last_heartbeat_at > first_beat_at,
        "last heartbeat moved forward: {} -> {}",
        first_beat_at,
        rows[0].last_heartbeat_at
    );
    drop_scratch_db(&pool, TEST_DB_UPSERT, &dir).await;
}

#[tokio::test]
async fn prune_drops_only_stale_rows_and_delete_removes_the_row() {
    let (pool, dir) = scratch_pool(TEST_DB_PRUNE, "prune").await;
    record_heartbeat(&pool, &row("host-1:100", 100, 0))
        .await
        .expect("live worker beats");
    record_heartbeat(&pool, &row("host-2:200", 200, 1))
        .await
        .expect("second worker beats");
    // Backdate the second worker's heartbeat by 2 h (a dead
    // worker) with a static literal — sqlx 0.9 forbids format! SQL.
    sqlx::query(
        r#"UPDATE worker_heartbeats
           SET last_heartbeat_at = now() - interval '2 hours'
           WHERE worker_id = $1"#,
    )
    .bind("host-2:200")
    .execute(&pool)
    .await
    .expect("backdate the dead worker");
    let pruned = prune_stale_heartbeats(&pool, HEARTBEAT_PRUNE_AFTER)
        .await
        .expect("prune runs");
    assert_eq!(pruned, 1, "exactly the stale row is pruned");
    let rows = list_heartbeats(&pool).await.expect("list reads");
    assert_eq!(rows.len(), 1, "only the live worker remains");
    assert_eq!(rows[0].worker_id, "host-1:100");
    // Graceful shutdown deletes the live worker's row.
    delete_heartbeat(&pool, "host-1:100")
        .await
        .expect("delete runs");
    let rows = list_heartbeats(&pool).await.expect("list reads again");
    assert!(rows.is_empty(), "no rows after the delete");
    drop_scratch_db(&pool, TEST_DB_PRUNE, &dir).await;
}
