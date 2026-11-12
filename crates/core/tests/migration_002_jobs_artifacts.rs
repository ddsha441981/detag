//! Live test for migration 002 — jobs + artifacts (P0-038, E0.4).
//!
//! Applies ONLY the 001 + 002 migration pairs (copied from the
//! repo's real `migrations/` into a scratch dir — later migrations
//! must not change what this test applies or reverts) to a scratch
//! DATABASE (`detag_p038_test`, created and dropped by the test) —
//! never the dev database. Asserts the schema from
//! docs/03-ARCHITECTURE/04-data-model.md (tables, CHECKs, unique
//! idempotency_key, 1-1 quality_reports, the doc's indexes) plus
//! the migration's revert, then cleans up. The `pf` role needs
//! CREATEDB (granted in the VM dev setup; CI's postgres service
//! user has it).

use detag_core::db::{connect, DbPool, PoolConfig};
use detag_core::migrate::{migrate_revert_last, migrate_up};
use std::path::PathBuf;

const TEST_DB: &str = "detag_p038_test";
const VERSION: i64 = 20261112064924;

fn database_url() -> String {
    std::env::var("PF_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://pf:pf@localhost:5432/pf".to_string())
}

fn test_database_url() -> String {
    let mut url = url::Url::parse(&database_url()).expect("PF_DATABASE_URL parses");
    url.set_path(&format!("/{TEST_DB}"));
    url.to_string()
}

fn repo_migrations_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("migrations")
}

/// A scratch dir holding ONLY the 001 + 002 pairs, copied from the
/// repo (002 depends on 001's users table). Later migrations must
/// not change what this test applies or reverts, so the test never
/// runs the whole `migrations/` dir: `migrate_revert_last` reverts
/// the last applied version, and that must be 002 here.
fn migrations_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("detag-p038-migrations-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch migrations dir");
    let repo = repo_migrations_dir();
    for stem in [
        "20261112064220_users_sessions",
        "20261112064924_jobs_artifacts",
    ] {
        for ext in ["up.sql", "down.sql"] {
            let name = format!("{stem}.{ext}");
            std::fs::copy(repo.join(&name), dir.join(&name)).expect("copy migration pair");
        }
    }
    dir
}

async fn table_exists(pool: &DbPool, table: &str) -> bool {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM information_schema.tables WHERE table_name = $1)",
    )
    .bind(table)
    .fetch_one(pool)
    .await
    .expect("check table exists")
}

async fn index_exists(pool: &DbPool, index: &str) -> bool {
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_indexes WHERE indexname = $1)")
        .bind(index)
        .fetch_one(pool)
        .await
        .expect("check index exists")
}

async fn create_test_db(admin_url: &str) {
    let admin = connect(admin_url, &PoolConfig::default())
        .await
        .unwrap_or_else(|err| {
            panic!(
                "live P0-038 test needs Postgres on {admin_url}: {err} — start it with `just dev` (or the native postgres, see DOCKER-001)"
            )
        });
    sqlx::query("DROP DATABASE IF EXISTS detag_p038_test")
        .execute(&admin)
        .await
        .expect("drop stale test db");
    sqlx::query("CREATE DATABASE detag_p038_test")
        .execute(&admin)
        .await
        .unwrap_or_else(|err| panic!("live P0-038 test needs CREATEDB for the pf role: {err}"));
    admin.close().await;
}

async fn drop_test_db(admin_url: &str) {
    let admin = connect(admin_url, &PoolConfig::default())
        .await
        .expect("reconnect for cleanup");
    sqlx::query("DROP DATABASE IF EXISTS detag_p038_test")
        .execute(&admin)
        .await
        .expect("drop test db");
    admin.close().await;
}

#[tokio::test]
async fn migration_002_jobs_artifacts_schema_indexes_and_cascade() {
    let url = database_url();
    create_test_db(&url).await;
    let pool = connect(&test_database_url(), &PoolConfig::default())
        .await
        .expect("connect to the scratch database");
    let mdir = migrations_dir();

    let applied = migrate_up(&pool, &mdir).await.expect("migrate up");
    assert!(
        applied.contains(&VERSION),
        "migration 002 must apply: {applied:?}"
    );
    for table in [
        "uploads",
        "jobs",
        "job_events",
        "artifacts",
        "quality_reports",
    ] {
        assert!(table_exists(&pool, table).await, "{table} must exist");
    }
    // Indexes from the data-model doc (minimum) + the FK/replay ones.
    for index in [
        "jobs_user_created_idx",
        "jobs_status_lane_priority_idx",
        "uploads_user_status_idx",
        "jobs_upload_id_idx",
        "job_events_job_at_idx",
        "artifacts_job_id_idx",
    ] {
        assert!(index_exists(&pool, index).await, "index {index} must exist");
    }

    let user_id: String = sqlx::query_scalar(
        "INSERT INTO users (email, password_hash) \
         VALUES ('p038@example.com', 'argon2id$placeholder') RETURNING id::text",
    )
    .fetch_one(&pool)
    .await
    .expect("insert user");

    // uploads kind CHECK.
    let bad_kind = sqlx::query(
        "INSERT INTO uploads (user_id, kind, filename, bytes, sha256, mime, storage_key) \
         VALUES ($1::uuid, 'audio', 'a.mp3', 1, 'h', 'audio/mpeg', 'k')",
    )
    .bind(user_id.clone())
    .execute(&pool)
    .await;
    assert!(
        bad_kind.is_err(),
        "upload kind outside image|video must fail"
    );

    let upload_id: String = sqlx::query_scalar(
        "INSERT INTO uploads (user_id, kind, filename, bytes, sha256, mime, storage_key) \
         VALUES ($1::uuid, 'image', 'a.png', 1024, 'deadbeef', 'image/png', 'u/1') RETURNING id::text",
    )
    .bind(user_id.clone())
    .fetch_one(&pool)
    .await
    .expect("insert upload");

    // jobs kind CHECK + idempotency_key unique.
    let bad_job_kind = sqlx::query(
        "INSERT INTO jobs (user_id, upload_id, kind, mode, status, lane) \
         VALUES ($1::uuid, $2::uuid, 'audio', 'inpaint', 'queued', 'standard')",
    )
    .bind(user_id.clone())
    .bind(upload_id.clone())
    .execute(&pool)
    .await;
    assert!(
        bad_job_kind.is_err(),
        "job kind outside image|video|batch must fail"
    );

    let job_id: String = sqlx::query_scalar(
        "INSERT INTO jobs (user_id, upload_id, kind, mode, status, lane, idempotency_key) \
         VALUES ($1::uuid, $2::uuid, 'image', 'inpaint', 'queued', 'standard', 'idem-1') RETURNING id::text",
    )
    .bind(user_id.clone())
    .bind(upload_id.clone())
    .fetch_one(&pool)
    .await
    .expect("insert job");
    let dup_idem = sqlx::query(
        "INSERT INTO jobs (user_id, kind, mode, status, lane, idempotency_key) \
         VALUES ($1::uuid, 'batch', 'inpaint', 'queued', 'standard', 'idem-1')",
    )
    .bind(user_id.clone())
    .execute(&pool)
    .await;
    assert!(dup_idem.is_err(), "duplicate idempotency_key must fail");

    // job_events type CHECK + artifacts type CHECK.
    let bad_event = sqlx::query("INSERT INTO job_events (job_id, type) VALUES ($1::uuid, 'shout')")
        .bind(job_id.clone())
        .execute(&pool)
        .await;
    assert!(
        bad_event.is_err(),
        "event type outside the doc list must fail"
    );
    sqlx::query(
        "INSERT INTO job_events (job_id, type, payload) VALUES ($1::uuid, 'stage', '{\"stage\": \"decode\"}')",
    )
    .bind(job_id.clone())
    .execute(&pool)
    .await
    .expect("insert job event");
    let bad_artifact = sqlx::query(
        "INSERT INTO artifacts (job_id, type, storage_key, bytes, sha256, expires_at) \
         VALUES ($1::uuid, 'thumbnail', 'a/1', 1, 'h', now() + interval '6 hours')",
    )
    .bind(job_id.clone())
    .execute(&pool)
    .await;
    assert!(
        bad_artifact.is_err(),
        "artifact type outside the doc list must fail"
    );
    sqlx::query(
        "INSERT INTO artifacts (job_id, type, storage_key, bytes, sha256, expires_at) \
         VALUES ($1::uuid, 'output', 'a/1', 2048, 'cafe', now() + interval '6 hours')",
    )
    .bind(job_id.clone())
    .execute(&pool)
    .await
    .expect("insert artifact");

    // quality_reports: 1-1 with jobs (second report fails).
    sqlx::query(
        "INSERT INTO quality_reports (job_id, engine_version, gate) \
         VALUES ($1::uuid, 'detag-engine 0.1.0', 'pass')",
    )
    .bind(job_id.clone())
    .execute(&pool)
    .await
    .expect("insert quality report");
    let dup_report = sqlx::query(
        "INSERT INTO quality_reports (job_id, engine_version, gate) \
         VALUES ($1::uuid, 'detag-engine 0.1.0', 'pass')",
    )
    .bind(job_id.clone())
    .execute(&pool)
    .await;
    assert!(
        dup_report.is_err(),
        "a job must have at most one quality report"
    );

    // CASCADE: deleting the user removes uploads, jobs, events,
    // artifacts and reports with it. (Literal queries: sqlx 0.9
    // rejects dynamically built SQL strings.)
    sqlx::query("DELETE FROM users WHERE id = $1::uuid")
        .bind(user_id.clone())
        .execute(&pool)
        .await
        .expect("delete user");
    let counts: [(i64, &str); 5] = [
        (
            sqlx::query_scalar("SELECT count(*) FROM uploads")
                .fetch_one(&pool)
                .await
                .expect("count uploads"),
            "uploads",
        ),
        (
            sqlx::query_scalar("SELECT count(*) FROM jobs")
                .fetch_one(&pool)
                .await
                .expect("count jobs"),
            "jobs",
        ),
        (
            sqlx::query_scalar("SELECT count(*) FROM job_events")
                .fetch_one(&pool)
                .await
                .expect("count job_events"),
            "job_events",
        ),
        (
            sqlx::query_scalar("SELECT count(*) FROM artifacts")
                .fetch_one(&pool)
                .await
                .expect("count artifacts"),
            "artifacts",
        ),
        (
            sqlx::query_scalar("SELECT count(*) FROM quality_reports")
                .fetch_one(&pool)
                .await
                .expect("count quality_reports"),
            "quality_reports",
        ),
    ];
    for (n, table) in counts {
        assert_eq!(n, 0, "{table} must be empty after the user delete");
    }

    // Revert 002: all five tables go away; re-apply restores them.
    let reverted = migrate_revert_last(&pool, &mdir).await.expect("revert 002");
    assert_eq!(reverted, Some(VERSION), "must revert 002");
    for table in [
        "uploads",
        "jobs",
        "job_events",
        "artifacts",
        "quality_reports",
    ] {
        assert!(!table_exists(&pool, table).await, "revert drops {table}");
    }
    let reapplied = migrate_up(&pool, &mdir).await.expect("re-apply 002");
    assert!(
        reapplied.contains(&VERSION),
        "re-apply must restore 002: {reapplied:?}"
    );

    pool.close().await;
    drop_test_db(&url).await;
    let _ = std::fs::remove_dir_all(&mdir);
}
