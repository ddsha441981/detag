//! Live test for the retry-event writer (P0-077, E0.8).
//!
//! Applies ONLY the 001 + 002 migration pairs plus the P1-005
//! profile-fields migration plus the P1-021 revoked-before
//! migration (copied from the repo's real `migrations/` into
//! a scratch dir — the same pattern as the migration live
//! tests) to a scratch DATABASE (`detag_p077_test`, created
//! and dropped by the test) — never the dev database. The
//! profile-fields migration is needed because the `users`
//! repo queries now read `display_name` + `pending_email`
//! (P1-005): without it, `users::create` fails with "column
//! display_name does not exist". The revoked-before migration
//! is needed for the same reason since P1-021 (`users` repo
//! queries now read `revoked_before`): without it,
//! `users::create` fails with "column revoked_before does
//! not exist". Creates a user + job through the repos,
//! records a retry event, reads it back, and asserts the
//! payload round-trips; also asserts the FK: recording for
//! a job id with no jobs row fails with `ForeignKeyViolation`
//! (the worker sink treats that as best-effort, never failing
//! the job).

use detag_core::db::{connect, DbPool, PoolConfig};
use detag_core::migrate::{migrate_revert_last, migrate_up};
use detag_core::repos::job_events::{
    delay_ms, record_retry_event, retry_events_for_job, RetryEventRow,
};
use detag_core::repos::jobs::{create as create_job, NewJob};
use detag_core::repos::users::create as create_user;
use detag_core::repos::RepoError;
use std::path::PathBuf;
use std::time::Duration;
use uuid::Uuid;

const TEST_DB_RETRY: &str = "detag_p077_retry";
const TEST_DB_FK: &str = "detag_p077_fk";

fn database_url() -> String {
    std::env::var("PF_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://pf:pf@localhost:5432/pf".to_string())
}

fn test_database_url(db: &str) -> String {
    let mut url = url::Url::parse(&database_url()).expect("PF_DATABASE_URL parses");
    url.set_path(&format!("/{db}"));
    url.to_string()
}

fn repo_migrations_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("migrations")
}

/// A scratch dir holding ONLY the 001 + 002 pairs + the P1-005
/// profile-fields pair + the P1-021 revoked-before pair,
/// copied from the repo (002 depends on 001's users table; the
/// profile-fields migration adds the columns the `users` repo
/// reads since P1-005; the revoked-before migration adds the
/// column it reads since P1-021). One dir per test (the tests
/// in this file run in parallel).
fn migrations_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "detag-p077-migrations-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch migrations dir");
    let repo = repo_migrations_dir();
    for stem in [
        "20261112064220_users_sessions",
        "20261112064924_jobs_artifacts",
        "20261113040605_user_profile_fields",
        "20261113120835_users_revoked_before",
    ] {
        for ext in ["up.sql", "down.sql"] {
            let name = format!("{stem}.{ext}");
            std::fs::copy(repo.join(&name), dir.join(&name)).expect("copy migration pair");
        }
    }
    dir
}

async fn scratch_pool(db: &str, tag: &str) -> (DbPool, PathBuf) {
    // Create the scratch database (drop a stale one first — a
    // previous interrupted run may have left it behind). The
    // names are static literals (the sqlx 0.9 dynamic-SQL lint
    // forbids format! here) — one literal per test db.
    let admin = connect(&database_url(), &PoolConfig::default())
        .await
        .expect("connect to dev db");
    let (drop_sql, create_sql): (&str, &str) = match db {
        TEST_DB_RETRY => (
            "DROP DATABASE IF EXISTS \"detag_p077_retry\"",
            "CREATE DATABASE \"detag_p077_retry\"",
        ),
        TEST_DB_FK => (
            "DROP DATABASE IF EXISTS \"detag_p077_fk\"",
            "CREATE DATABASE \"detag_p077_fk\"",
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
    // Revert the last applied migration (now the P1-005
    // profile-fields one), then drop the db.
    let _ = migrate_revert_last(pool, dir).await;
    pool.close().await;
    let admin = connect(&database_url(), &PoolConfig::default())
        .await
        .expect("connect to dev db");
    let drop_sql: &str = match db {
        TEST_DB_RETRY => "DROP DATABASE IF EXISTS \"detag_p077_retry\"",
        TEST_DB_FK => "DROP DATABASE IF EXISTS \"detag_p077_fk\"",
        _ => panic!("unknown scratch db {db}"),
    };
    sqlx::query(drop_sql)
        .execute(&admin)
        .await
        .expect("drop scratch db");
    admin.close().await;
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn retry_event_round_trips_through_job_events() {
    let (pool, dir) = scratch_pool(TEST_DB_RETRY, "retry").await;
    let user = create_user(&pool, "p077@example.com", "hash-placeholder")
        .await
        .expect("create user");
    let job = create_job(
        &pool,
        &NewJob {
            user_id: user.id,
            upload_id: None,
            kind: "image",
            mode: "full",
            status: "queued",
            lane: "free",
            params: serde_json::json!({}),
            idempotency_key: None,
        },
    )
    .await
    .expect("create job");
    let event = RetryEventRow {
        attempt: 1,
        delay_ms: delay_ms(Duration::from_secs(5 * 60)),
        error: "boom".to_string(),
    };
    let id = record_retry_event(&pool, job.id, &event)
        .await
        .expect("record retry event");
    assert_ne!(id, Uuid::nil(), "the event row gets an id");
    let events = retry_events_for_job(&pool, job.id)
        .await
        .expect("read retry events");
    assert_eq!(events.len(), 1, "exactly one retry event");
    assert_eq!(events[0]["event"], "retry");
    assert_eq!(events[0]["attempt"], 1);
    assert_eq!(events[0]["delay_ms"], 5 * 60 * 1000);
    assert_eq!(events[0]["error"], "boom");
    drop_scratch_db(&pool, TEST_DB_RETRY, &dir).await;
}

#[tokio::test]
async fn retry_event_for_unknown_job_fails_foreign_key() {
    let (pool, dir) = scratch_pool(TEST_DB_FK, "fk").await;
    let err = record_retry_event(
        &pool,
        Uuid::now_v7(),
        &RetryEventRow {
            attempt: 0,
            delay_ms: 60_000,
            error: "boom".to_string(),
        },
    )
    .await
    .expect_err("no jobs row — the FK must fail");
    assert!(
        matches!(err, RepoError::ForeignKeyViolation { .. }),
        "expected ForeignKeyViolation, got {err:?}"
    );
    drop_scratch_db(&pool, TEST_DB_FK, &dir).await;
}
