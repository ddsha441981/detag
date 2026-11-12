//! Live transaction-helper tests (P0-042, E0.4).
//!
//! Exercises `tx::with_tx` against a real Postgres on
//! `PF_DATABASE_URL` (default: the dev database) using a scratch
//! DATABASE per test (`detag_p042_*`, created and dropped by the test) with
//! all repo migrations applied — never the dev database. Covers
//! commit-on-success, rollback-on-error (the ticket's AC), the
//! nesting guard, and the statement timeout. The `pf` role needs
//! CREATEDB (granted in the VM dev setup; CI's postgres service
//! user has it).

use detag_core::db::{connect, DbPool, PoolConfig};
use detag_core::migrate::migrate_up;
use detag_core::repos::error::RepoError;
use detag_core::repos::{jobs, users};
use detag_core::tx::{with_tx, TxError};
use std::path::PathBuf;
use std::time::Duration;

fn database_url() -> String {
    std::env::var("PF_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://pf:pf@localhost:5432/pf".to_string())
}

fn test_database_url(db: &str) -> String {
    let mut url = url::Url::parse(&database_url()).expect("PF_DATABASE_URL parses");
    url.set_path(&format!("/{db}"));
    url.to_string()
}

/// DDL cannot take bind parameters, and sqlx 0.9 rejects
/// dynamically built SQL strings — so the DROP/CREATE statements
/// are literals selected by the (known, fixed) test DB name.
fn drop_db_sql(db: &str) -> &'static str {
    match db {
        "detag_p042_commit" => "DROP DATABASE IF EXISTS detag_p042_commit",
        "detag_p042_rollback" => "DROP DATABASE IF EXISTS detag_p042_rollback",
        "detag_p042_nesting" => "DROP DATABASE IF EXISTS detag_p042_nesting",
        "detag_p042_timeout" => "DROP DATABASE IF EXISTS detag_p042_timeout",
        _ => panic!("unknown P0-042 test db {db}"),
    }
}

fn create_db_sql(db: &str) -> &'static str {
    match db {
        "detag_p042_commit" => "CREATE DATABASE detag_p042_commit",
        "detag_p042_rollback" => "CREATE DATABASE detag_p042_rollback",
        "detag_p042_nesting" => "CREATE DATABASE detag_p042_nesting",
        "detag_p042_timeout" => "CREATE DATABASE detag_p042_timeout",
        _ => panic!("unknown P0-042 test db {db}"),
    }
}

fn migrations_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("migrations")
}

async fn setup(db: &str) -> DbPool {
    let url = database_url();
    let admin = connect(&url, &PoolConfig::default())
        .await
        .unwrap_or_else(|err| {
            panic!(
                "live P0-042 test needs Postgres on {url}: {err} — start it with `just dev` (or the native postgres, see DOCKER-001)"
            )
        });
    sqlx::query(drop_db_sql(db))
        .execute(&admin)
        .await
        .expect("drop stale test db");
    sqlx::query(create_db_sql(db))
        .execute(&admin)
        .await
        .unwrap_or_else(|err| panic!("live P0-042 test needs CREATEDB for the pf role: {err}"));
    admin.close().await;

    let pool = connect(&test_database_url(db), &PoolConfig::default())
        .await
        .expect("connect to the scratch database");
    migrate_up(&pool, &migrations_dir())
        .await
        .expect("migrate up");
    pool
}

async fn teardown(pool: DbPool, db: &str) {
    pool.close().await;
    let admin = connect(&database_url(), &PoolConfig::default())
        .await
        .expect("reconnect for cleanup");
    sqlx::query(drop_db_sql(db))
        .execute(&admin)
        .await
        .expect("drop test db");
    admin.close().await;
}

async fn count_users(pool: &DbPool, email: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM users WHERE email = $1")
        .bind(email)
        .fetch_one(pool)
        .await
        .expect("count users")
}

#[tokio::test]
async fn commit_on_success_writes_both_rows() {
    let pool = setup("detag_p042_commit").await;
    let timeout = PoolConfig::default().transaction_timeout;

    let (user_id, job_id) = with_tx(&pool, timeout, |conn: &mut sqlx::PgConnection| {
        Box::pin(async move {
            let user = users::create(&mut *conn, "tx-ok@example.com", "h").await?;
            let job = jobs::create(
                &mut *conn,
                &jobs::NewJob {
                    user_id: user.id,
                    upload_id: None,
                    kind: "image",
                    mode: "inpaint",
                    status: "queued",
                    lane: "standard",
                    params: serde_json::json!({}),
                    idempotency_key: None,
                },
            )
            .await?;
            Ok::<_, RepoError>((user.id, job.id))
        })
    })
    .await
    .expect("with_tx must commit");

    assert_eq!(count_users(&pool, "tx-ok@example.com").await, 1);
    assert!(jobs::by_id(&pool, job_id).await.expect("by_id").is_some());
    let _ = user_id;

    teardown(pool, "detag_p042_commit").await;
}

#[tokio::test]
async fn rollback_on_error_writes_nothing() {
    let pool = setup("detag_p042_rollback").await;
    let timeout = PoolConfig::default().transaction_timeout;

    let err = with_tx(&pool, timeout, |conn: &mut sqlx::PgConnection| {
        Box::pin(async move {
            users::create(&mut *conn, "tx-rollback@example.com", "h").await?;
            Err::<(), RepoError>(RepoError::NotFound)
        })
    })
    .await
    .expect_err("with_tx must report the closure's error");
    assert!(
        matches!(err, TxError::Inner(RepoError::NotFound)),
        "expected Inner(NotFound), got {err:?}"
    );
    assert_eq!(count_users(&pool, "tx-rollback@example.com").await, 0);

    teardown(pool, "detag_p042_rollback").await;
}

#[tokio::test]
async fn nesting_guard_fails_the_inner_call() {
    let pool = setup("detag_p042_nesting").await;
    let timeout = PoolConfig::default().transaction_timeout;

    // The inner call needs its own pool handle: the outer call
    // borrows `pool` while the closure runs, so the closure cannot
    // also borrow it. (A pool handle is cheap — it is an Arc.)
    let pool2 = pool.clone();
    let result = with_tx(&pool, timeout, |conn: &mut sqlx::PgConnection| {
        Box::pin(async move {
            // The pool is captured from the outer scope: a second
            // with_tx here must fail Nested, not open a transaction.
            let inner = with_tx(&pool2, timeout, |conn2: &mut sqlx::PgConnection| {
                Box::pin(
                    async move { users::create(&mut *conn2, "tx-nested@example.com", "h").await },
                )
            })
            .await;
            assert!(
                matches!(inner, Err(TxError::Nested)),
                "inner with_tx must fail Nested, got {inner:?}"
            );
            // The outer transaction is unaffected and commits.
            users::create(&mut *conn, "tx-outer@example.com", "h").await
        })
    })
    .await;
    assert!(
        result.is_ok(),
        "outer with_tx must still commit: {result:?}"
    );
    assert_eq!(count_users(&pool, "tx-outer@example.com").await, 1);
    assert_eq!(count_users(&pool, "tx-nested@example.com").await, 0);

    teardown(pool, "detag_p042_nesting").await;
}

#[tokio::test]
async fn statement_timeout_cancels_a_slow_statement() {
    let pool = setup("detag_p042_timeout").await;

    let err = with_tx(
        &pool,
        Duration::from_millis(10),
        |conn: &mut sqlx::PgConnection| {
            Box::pin(async move {
                sqlx::query("SELECT pg_sleep(1.0)")
                    .execute(&mut *conn)
                    .await
                    .map_err(RepoError::from)
            })
        },
    )
    .await
    .expect_err("a 1s statement under a 10ms timeout must fail");
    match err {
        TxError::Inner(RepoError::Db(sqlx::Error::Database(db_err))) => {
            // 57014 query_canceled: the statement timeout fired.
            assert_eq!(db_err.code().as_deref(), Some("57014"));
        }
        other => panic!("expected a canceled-statement error, got {other:?}"),
    }

    teardown(pool, "detag_p042_timeout").await;
}
