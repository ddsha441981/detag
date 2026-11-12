//! Live migration tests (P0-036, E0.4).
//!
//! Runs `migrate_up` / `migrate_revert_last` against a real Postgres
//! on `PF_DATABASE_URL` (default: the dev database) using a scratch
//! migrations dir — never the repo's `migrations/` — and a scratch
//! DATABASE (`detag_migrate_test_p036`, created and dropped by the
//! test), so the dev database's `_sqlx_migrations` is never touched:
//! sqlx fails a later `just migrate` when applied versions are
//! missing from the source (`VersionMissing`), so test rows must not
//! leak into the real database. The `pf` role needs CREATEDB (granted
//! in the VM dev setup; CI's postgres service user has it).

use detag_core::db::{connect, DbPool, PoolConfig};
use detag_core::migrate::{create_migration, migrate_revert_last, migrate_up, MigrateToolError};
use std::path::PathBuf;

const TABLE: &str = "migrate_test_table_p036";
const TEST_DB: &str = "detag_migrate_test_p036";

fn database_url() -> String {
    std::env::var("PF_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://pf:pf@localhost:5432/pf".to_string())
}

/// Same server, different database: swap the URL path.
fn test_database_url() -> String {
    let mut url = url::Url::parse(&database_url()).expect("PF_DATABASE_URL parses");
    url.set_path(&format!("/{TEST_DB}"));
    url.to_string()
}

fn scratch_dir() -> PathBuf {
    std::env::temp_dir().join(format!("detag-migrate-live-{}", std::process::id()))
}

fn write_migration(dir: &std::path::Path, name: &str, up_sql: &str, down_sql: &str) {
    let (up, down) =
        create_migration(dir, name, chrono::Utc::now()).expect("create migration pair");
    std::fs::write(&up, up_sql).expect("write up sql");
    std::fs::write(&down, down_sql).expect("write down sql");
}

async fn table_exists(pool: &DbPool) -> bool {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM information_schema.tables WHERE table_name = 'migrate_test_table_p036')",
    )
    .fetch_one(pool)
    .await
    .expect("check table exists")
}

#[tokio::test]
async fn migrate_up_and_revert_round_trip() {
    let dir = scratch_dir();
    let _ = std::fs::remove_dir_all(&dir);
    let url = database_url();

    // Admin pool (the dev database) only to create/drop the scratch DB.
    let admin = connect(&url, &PoolConfig::default())
        .await
        .unwrap_or_else(|err| {
            panic!(
                "live migrate test needs Postgres on {url}: {err} — start it with `just dev` (or the native postgres, see DOCKER-001)"
            )
        });
    sqlx::query("DROP DATABASE IF EXISTS detag_migrate_test_p036")
        .execute(&admin)
        .await
        .expect("drop stale test db");
    sqlx::query("CREATE DATABASE detag_migrate_test_p036")
        .execute(&admin)
        .await
        .unwrap_or_else(|err| panic!("live migrate test needs CREATEDB for the pf role: {err}"));
    admin.close().await;

    let pool = connect(&test_database_url(), &PoolConfig::default())
        .await
        .expect("connect to the scratch database");

    // One simple up-only migration (version 1) + one reversible pair.
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    std::fs::write(dir.join("0001_simple.sql"), "SELECT 1;").expect("write simple migration");
    write_migration(
        &dir,
        "reversible table",
        &format!("CREATE TABLE {TABLE} (id INT PRIMARY KEY);"),
        &format!("DROP TABLE {TABLE};"),
    );

    // Up: both apply; second run applies nothing (idempotent).
    let applied = migrate_up(&pool, &dir).await.expect("migrate up");
    assert_eq!(applied.len(), 2, "both migrations apply: {applied:?}");
    assert!(
        table_exists(&pool).await,
        "reversible migration must create the table"
    );
    let applied_again = migrate_up(&pool, &dir).await.expect("migrate up again");
    assert!(
        applied_again.is_empty(),
        "second run must be a no-op: {applied_again:?}"
    );

    // Revert the last (reversible) one: the table goes away.
    let reverted = migrate_revert_last(&pool, &dir).await.expect("revert last");
    assert!(reverted.is_some(), "something must revert");
    assert!(!table_exists(&pool).await, "revert must drop the table");

    // Re-apply, then revert again: now the last applied migration is
    // the simple up-only one — revert must refuse, not silently pass.
    migrate_up(&pool, &dir).await.expect("migrate up again");
    // Revert the reversible one first (it is last again).
    migrate_revert_last(&pool, &dir)
        .await
        .expect("revert reversible again");
    let err = migrate_revert_last(&pool, &dir)
        .await
        .expect_err("reverting an up-only migration must fail");
    assert!(
        matches!(err, MigrateToolError::NotReversible(1)),
        "expected NotReversible(1), got {err}"
    );

    // Cleanup: drop the scratch database and dir.
    pool.close().await;
    let admin = connect(&url, &PoolConfig::default())
        .await
        .expect("reconnect for cleanup");
    sqlx::query("DROP DATABASE IF EXISTS detag_migrate_test_p036")
        .execute(&admin)
        .await
        .expect("drop test db");
    admin.close().await;
    let _ = std::fs::remove_dir_all(&dir);
}
