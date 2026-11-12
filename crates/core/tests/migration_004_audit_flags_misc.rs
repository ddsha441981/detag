//! Live test for migration 004 — audit + flags + misc (P0-040, E0.4).
//!
//! Applies the 0001 + 001 + 002 + 003 + 004 migration files (copied
//! from the repo's real `migrations/` into a scratch dir — later
//! migrations must not change what this test applies or reverts) to
//! a scratch DATABASE (`detag_p040_test`, created and dropped by
//! the test) — never the dev database. Asserts the tables and the
//! doc's indexes, the append-only triggers (audit_logs, job_events,
//! attestations), the deferred jobs.attestation_id FK, plus the
//! migration's revert, then cleans up. The `pf` role needs CREATEDB
//! (granted in the VM dev setup; CI's postgres service user has it).

use detag_core::db::{connect, DbPool, PoolConfig};
use detag_core::migrate::{migrate_revert_last, migrate_up};
use std::path::PathBuf;

const TEST_DB: &str = "detag_p040_test";
const VERSION: i64 = 20261112065339;

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

fn migrations_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("detag-p040-migrations-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch migrations dir");
    let repo = repo_migrations_dir();
    // 0001 is a single up-only file, not a pair.
    std::fs::copy(
        repo.join("0001_feature_flags.sql"),
        dir.join("0001_feature_flags.sql"),
    )
    .expect("copy 0001");
    for stem in [
        "20261112064220_users_sessions",
        "20261112064924_jobs_artifacts",
        "20261112065140_billing",
        "20261112065339_audit_flags_misc",
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
                "live P0-040 test needs Postgres on {admin_url}: {err} — start it with `just dev` (or the native postgres, see DOCKER-001)"
            )
        });
    sqlx::query("DROP DATABASE IF EXISTS detag_p040_test")
        .execute(&admin)
        .await
        .expect("drop stale test db");
    sqlx::query("CREATE DATABASE detag_p040_test")
        .execute(&admin)
        .await
        .unwrap_or_else(|err| panic!("live P0-040 test needs CREATEDB for the pf role: {err}"));
    admin.close().await;
}

async fn drop_test_db(admin_url: &str) {
    let admin = connect(admin_url, &PoolConfig::default())
        .await
        .expect("reconnect for cleanup");
    sqlx::query("DROP DATABASE IF EXISTS detag_p040_test")
        .execute(&admin)
        .await
        .expect("drop test db");
    admin.close().await;
}

#[tokio::test]
async fn migration_004_audit_flags_misc_schema_triggers_and_fk() {
    let url = database_url();
    create_test_db(&url).await;
    let pool = connect(&test_database_url(), &PoolConfig::default())
        .await
        .expect("connect to the scratch database");
    let mdir = migrations_dir();

    let applied = migrate_up(&pool, &mdir).await.expect("migrate up");
    assert!(
        applied.contains(&VERSION),
        "migration 004 must apply: {applied:?}"
    );
    for table in [
        "feature_flags",
        "audit_logs",
        "idempotency_keys",
        "email_outbox",
        "attestations",
        "abuse_reports",
        "webhook_endpoints",
        "webhook_deliveries",
        "api_keys",
    ] {
        assert!(table_exists(&pool, table).await, "{table} must exist");
    }
    for index in [
        "audit_logs_actor_created_idx",
        "audit_logs_target_idx",
        "idempotency_keys_expires_at_idx",
        "webhook_deliveries_endpoint_created_idx",
        "api_keys_prefix_idx",
        "email_outbox_due_idx",
    ] {
        assert!(index_exists(&pool, index).await, "index {index} must exist");
    }

    let user_id: String = sqlx::query_scalar(
        "INSERT INTO users (email, password_hash) \
         VALUES ('p040@example.com', 'argon2id$placeholder') RETURNING id::text",
    )
    .fetch_one(&pool)
    .await
    .expect("insert user");

    // Append-only: UPDATE/DELETE on audit_logs, job_events and
    // attestations must fail with the trigger's message.
    sqlx::query(
        "INSERT INTO audit_logs (actor_user_id, action, target_type, target_id) \
         VALUES ($1::uuid, 'user.login', 'users', $1)",
    )
    .bind(user_id.clone())
    .execute(&pool)
    .await
    .expect("insert audit log");
    for sql in [
        "UPDATE audit_logs SET action = 'tampered'",
        "DELETE FROM audit_logs",
    ] {
        let err = sqlx::query(sql).execute(&pool).await;
        assert!(err.is_err(), "{sql} on audit_logs must fail");
        assert!(
            err.expect_err("audit log mutation must fail")
                .to_string()
                .contains("append-only"),
            "audit log mutation must fail on the append-only trigger"
        );
    }

    let upload_id: String = sqlx::query_scalar(
        "INSERT INTO uploads (user_id, kind, filename, bytes, sha256, mime, storage_key) \
         VALUES ($1::uuid, 'image', 'a.png', 1, 'h', 'image/png', 'k') RETURNING id::text",
    )
    .bind(user_id.clone())
    .fetch_one(&pool)
    .await
    .expect("insert upload");
    let job_id: String = sqlx::query_scalar(
        "INSERT INTO jobs (user_id, upload_id, kind, mode, status, lane) \
         VALUES ($1::uuid, $2::uuid, 'image', 'inpaint', 'queued', 'standard') RETURNING id::text",
    )
    .bind(user_id.clone())
    .bind(upload_id.clone())
    .fetch_one(&pool)
    .await
    .expect("insert job");
    sqlx::query("INSERT INTO job_events (job_id, type) VALUES ($1::uuid, 'stage')")
        .bind(job_id.clone())
        .execute(&pool)
        .await
        .expect("insert job event");
    for sql in [
        "UPDATE job_events SET type = 'note'",
        "DELETE FROM job_events",
    ] {
        let err = sqlx::query(sql).execute(&pool).await;
        assert!(err.is_err(), "{sql} on job_events must fail");
    }

    // Attestations: immutable rows + the deferred jobs FK.
    let att_id: String = sqlx::query_scalar(
        "INSERT INTO attestations (user_id, upload_id, text_version, ip_hash, client) \
         VALUES ($1::uuid, $2::uuid, 'v1', 'hash123', 'web') RETURNING id::text",
    )
    .bind(user_id.clone())
    .bind(upload_id.clone())
    .fetch_one(&pool)
    .await
    .expect("insert attestation");
    let err = sqlx::query("UPDATE attestations SET text_version = 'v2'")
        .execute(&pool)
        .await;
    assert!(err.is_err(), "attestations must be immutable");
    let bad_fk = sqlx::query(
        "INSERT INTO jobs (user_id, kind, mode, status, lane, attestation_id) \
         VALUES ($1::uuid, 'image', 'inpaint', 'queued', 'standard', '00000000-0000-0000-0000-000000000000')",
    )
    .bind(user_id.clone())
    .execute(&pool)
    .await;
    assert!(bad_fk.is_err(), "job with an unknown attestation must fail");
    sqlx::query("UPDATE jobs SET attestation_id = $1::uuid WHERE id = $2::uuid")
        .bind(att_id.clone())
        .bind(job_id.clone())
        .execute(&pool)
        .await
        .expect("link job to attestation");
    // The attestation can never be deleted while the immutability
    // trigger stands, so the FK's ON DELETE SET NULL is
    // belt-and-braces for a hypothetical privileged purge — it is
    // deliberately NOT exercised here; the trigger is.
    let delete_att = sqlx::query("DELETE FROM attestations WHERE id = $1::uuid")
        .bind(att_id.clone())
        .execute(&pool)
        .await;
    assert!(delete_att.is_err(), "attestations DELETE must fail");
    let _ = att_id;

    // abuse_reports status CHECK; idempotency_keys PK; api_keys +
    // webhook endpoints/deliveries roundtrip.
    let bad_status = sqlx::query(
        "INSERT INTO abuse_reports (job_id, reason, status) VALUES ($1::uuid, 'spam', 'ignored')",
    )
    .bind(job_id.clone())
    .execute(&pool)
    .await;
    assert!(
        bad_status.is_err(),
        "abuse status outside open|reviewing|resolved must fail"
    );
    sqlx::query(
        "INSERT INTO idempotency_keys (key, user_id, endpoint, expires_at) \
         VALUES ('idem-004', $1::uuid, '/v1/jobs', now() + interval '1 day')",
    )
    .bind(user_id.clone())
    .execute(&pool)
    .await
    .expect("insert idempotency key");
    let dup_key = sqlx::query(
        "INSERT INTO idempotency_keys (key, user_id, endpoint, expires_at) \
         VALUES ('idem-004', $1::uuid, '/v1/jobs', now() + interval '1 day')",
    )
    .bind(user_id.clone())
    .execute(&pool)
    .await;
    assert!(dup_key.is_err(), "duplicate idempotency key must fail");
    let endpoint_id: String = sqlx::query_scalar(
        "INSERT INTO webhook_endpoints (user_id, url, secret_hash, events) \
         VALUES ($1::uuid, 'https://example.com/hook', 'sechash', ARRAY['job.done']) RETURNING id::text",
    )
    .bind(user_id.clone())
    .fetch_one(&pool)
    .await
    .expect("insert webhook endpoint");
    sqlx::query(
        "INSERT INTO webhook_deliveries (endpoint_id, event_id, event_type) \
         VALUES ($1::uuid, 'evt-1', 'job.done')",
    )
    .bind(endpoint_id.clone())
    .execute(&pool)
    .await
    .expect("insert webhook delivery");
    sqlx::query(
        "INSERT INTO api_keys (user_id, name, prefix, secret_hash, scopes) \
         VALUES ($1::uuid, 'ci key', 'dtg_live_', 'hash', ARRAY['jobs:read'])",
    )
    .bind(user_id.clone())
    .execute(&pool)
    .await
    .expect("insert api key");

    // Revert 004: tables + triggers + function + jobs FK go away;
    // re-apply restores them. (feature_flags/0001 is untouched by
    // the 004 down migration — it stays.)
    let reverted = migrate_revert_last(&pool, &mdir).await.expect("revert 004");
    assert_eq!(reverted, Some(VERSION), "must revert 004");
    for table in [
        "audit_logs",
        "idempotency_keys",
        "email_outbox",
        "attestations",
        "abuse_reports",
        "webhook_endpoints",
        "webhook_deliveries",
        "api_keys",
    ] {
        assert!(!table_exists(&pool, table).await, "revert drops {table}");
    }
    assert!(
        table_exists(&pool, "feature_flags").await,
        "revert keeps feature_flags"
    );
    let reapplied = migrate_up(&pool, &mdir).await.expect("re-apply 004");
    assert!(
        reapplied.contains(&VERSION),
        "re-apply must restore 004: {reapplied:?}"
    );

    pool.close().await;
    drop_test_db(&url).await;
    let _ = std::fs::remove_dir_all(&mdir);
}
