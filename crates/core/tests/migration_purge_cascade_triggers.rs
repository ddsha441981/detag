//! Live test for migration 20261113062352 — purge cascade
//! triggers (P1-007).
//!
//! Applies the repo's migrations up to and including the purge
//! migration (copied from the repo's real `migrations/` into a
//! scratch dir — the list is pinned, like the 004 test's, so a
//! later migration does not change what this test applies or
//! reverts) to a scratch DATABASE (`detag_purge_test`, created
//! and dropped by the test) — never the dev database. Asserts
//! the new contract:
//!
//! - `audit_logs.actor_user_id` has no FK (the actor is a
//!   historical fact, not a live reference — the purge leaves
//!   the actor id intact, and an audit row may even name an
//!   actor that no longer exists);
//! - `job_events` / `attestations` triggers are UPDATE-only
//!   (history can never be rewritten; DELETE is the purge
//!   cascade's erasure path);
//! - `audit_logs` trigger still forbids UPDATE and DELETE (the
//!   audit trail is never deleted);
//! - deleting a user cascades through jobs/uploads/job_events/
//!   attestations while the audit row survives with its actor
//!   intact;
//! - the down migration restores the old contract (FK back,
//!   triggers back to UPDATE OR DELETE), and re-applying
//!   restores the new one.
//!
//! The `pf` role needs CREATEDB (granted in the VM dev setup;
//! CI's postgres service user has it).

use detag_core::db::{connect, DbPool, PoolConfig};
use detag_core::migrate::{migrate_revert_last, migrate_up};
use std::path::PathBuf;

const TEST_DB: &str = "detag_purge_test";
const VERSION: i64 = 20261113062352;

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
    let dir = std::env::temp_dir().join(format!("detag-purge-migrations-{}", std::process::id()));
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
        "20261112133104_worker_heartbeats",
        "20261113014639_email_verification_tokens",
        "20261113030740_login_attempt_counters",
        "20261113040605_user_profile_fields",
        "20261113050515_email_change_token_purpose",
        "20261113062352_purge_cascade_triggers",
    ] {
        for ext in ["up.sql", "down.sql"] {
            let name = format!("{stem}.{ext}");
            std::fs::copy(repo.join(&name), dir.join(&name)).expect("copy migration pair");
        }
    }
    dir
}

async fn create_test_db(admin_url: &str) {
    let admin = connect(admin_url, &PoolConfig::default())
        .await
        .unwrap_or_else(|err| {
            panic!(
                "live purge-migration test needs Postgres on {admin_url}: {err} — start it with `just dev` (or the native postgres, see DOCKER-001)"
            )
        });
    sqlx::query("DROP DATABASE IF EXISTS detag_purge_test")
        .execute(&admin)
        .await
        .expect("drop stale test db");
    sqlx::query("CREATE DATABASE detag_purge_test")
        .execute(&admin)
        .await
        .unwrap_or_else(|err| {
            panic!("live purge-migration test needs CREATEDB for the pf role: {err}")
        });
    admin.close().await;
}

async fn drop_test_db(admin_url: &str) {
    let admin = connect(admin_url, &PoolConfig::default())
        .await
        .expect("reconnect for cleanup");
    sqlx::query("DROP DATABASE IF EXISTS detag_purge_test")
        .execute(&admin)
        .await
        .expect("drop test db");
    admin.close().await;
}

/// The trigger definition for a table (`BEFORE UPDATE ...` vs
/// `BEFORE DELETE OR UPDATE ...`).
async fn trigger_def(pool: &DbPool, trigger: &str) -> String {
    sqlx::query_scalar("SELECT pg_get_triggerdef(oid) FROM pg_trigger WHERE tgname = $1")
        .bind(trigger)
        .fetch_one(pool)
        .await
        .expect("trigger def")
}

/// How many FK constraints `audit_logs.actor_user_id` has.
async fn actor_fk_count(pool: &DbPool) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM pg_constraint \
         WHERE conrelid = 'audit_logs'::regclass AND contype = 'f'",
    )
    .fetch_one(pool)
    .await
    .expect("fk count")
}

#[tokio::test]
async fn migration_purge_cascade_triggers_contract() {
    let url = database_url();
    create_test_db(&url).await;
    let pool = connect(&test_database_url(), &PoolConfig::default())
        .await
        .expect("connect to the scratch database");
    let mdir = migrations_dir();

    let applied = migrate_up(&pool, &mdir).await.expect("migrate up");
    assert!(
        applied.contains(&VERSION),
        "purge migration must apply: {applied:?}"
    );

    // 1. The audit actor FK is gone (the column stays a plain
    //    UUID — an audit row may name an actor that no longer
    //    exists; that is the point).
    assert_eq!(
        actor_fk_count(&pool).await,
        0,
        "audit actor FK must be gone"
    );
    let ghost = "11111111-1111-4111-8111-111111111111";
    sqlx::query(
        "INSERT INTO audit_logs (actor_user_id, action, target_type, target_id) \
         VALUES ($1::uuid, 'test.ghost', 'user', 'ghost')",
    )
    .bind(ghost)
    .execute(&pool)
    .await
    .expect("audit row may name a nonexistent actor");

    // 2. The triggers: audit_logs still UPDATE OR DELETE;
    //    job_events / attestations UPDATE-only.
    let audit_def = trigger_def(&pool, "audit_logs_append_only").await;
    assert!(
        audit_def.contains("DELETE"),
        "audit trigger keeps DELETE: {audit_def}"
    );
    assert!(
        audit_def.contains("UPDATE"),
        "audit trigger keeps UPDATE: {audit_def}"
    );
    for trigger in ["job_events_append_only", "attestations_immutable"] {
        let def = trigger_def(&pool, trigger).await;
        assert!(def.contains("UPDATE"), "{trigger} keeps UPDATE: {def}");
        assert!(!def.contains("DELETE"), "{trigger} drops DELETE: {def}");
    }

    // Seed a user with history: an audit row naming them, an
    // upload, a job, a job event, an attestation linked to the
    // job.
    let user_id: String = sqlx::query_scalar(
        "INSERT INTO users (email, password_hash) \
         VALUES ('purge-mig@example.com', 'argon2id$placeholder') RETURNING id::text",
    )
    .fetch_one(&pool)
    .await
    .expect("insert user");
    sqlx::query(
        "INSERT INTO audit_logs (actor_user_id, action, target_type, target_id) \
         VALUES ($1::uuid, 'user.login', 'users', $1)",
    )
    .bind(user_id.clone())
    .execute(&pool)
    .await
    .expect("insert audit log");
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
    let att_id: String = sqlx::query_scalar(
        "INSERT INTO attestations (user_id, upload_id, text_version, ip_hash, client) \
         VALUES ($1::uuid, $2::uuid, 'v1', 'hash123', 'web') RETURNING id::text",
    )
    .bind(user_id.clone())
    .bind(upload_id.clone())
    .fetch_one(&pool)
    .await
    .expect("insert attestation");
    sqlx::query("UPDATE jobs SET attestation_id = $1::uuid WHERE id = $2::uuid")
        .bind(att_id.clone())
        .bind(job_id.clone())
        .execute(&pool)
        .await
        .expect("link job to attestation");

    // 3. The contract on live rows: UPDATE fails everywhere
    //    (history is never rewritten); DELETE fails on
    //    audit_logs only; DELETE succeeds on job_events and
    //    attestations (the purge erasure path).
    for sql in [
        "UPDATE audit_logs SET action = 'tampered'",
        "DELETE FROM audit_logs",
        "UPDATE job_events SET type = 'note'",
        "UPDATE attestations SET text_version = 'v2'",
    ] {
        let err = sqlx::query(sql).execute(&pool).await;
        assert!(err.is_err(), "{sql} must fail");
    }
    for sql in ["DELETE FROM job_events", "DELETE FROM attestations"] {
        assert!(
            sqlx::query(sql).execute(&pool).await.is_ok(),
            "{sql} must succeed"
        );
    }
    // Re-seed the two rows the DELETEs just removed (the
    // cascade check below needs them present).
    sqlx::query("INSERT INTO job_events (job_id, type) VALUES ($1::uuid, 'stage')")
        .bind(job_id.clone())
        .execute(&pool)
        .await
        .expect("re-insert job event");
    let att_id: String = sqlx::query_scalar(
        "INSERT INTO attestations (user_id, upload_id, text_version, ip_hash, client) \
         VALUES ($1::uuid, $2::uuid, 'v1', 'hash123', 'web') RETURNING id::text",
    )
    .bind(user_id.clone())
    .bind(upload_id.clone())
    .fetch_one(&pool)
    .await
    .expect("re-insert attestation");
    let _ = att_id;

    // 4. The purge cascade: deleting the user deletes the
    //    upload, job, job event and attestation — and the audit
    //    row SURVIVES with its actor intact. (Explicit literal
    //    queries — sqlx 0.9 rejects dynamically built SQL.)
    sqlx::query("DELETE FROM users WHERE id = $1::uuid")
        .bind(user_id.clone())
        .execute(&pool)
        .await
        .expect("purge the user");
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE id = $1::uuid")
        .bind(user_id.clone())
        .fetch_one(&pool)
        .await
        .expect("count users after purge");
    assert_eq!(n, 0, "user row must be gone after the purge");
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM uploads WHERE id = $1::uuid")
        .bind(upload_id.clone())
        .fetch_one(&pool)
        .await
        .expect("count uploads after purge");
    assert_eq!(n, 0, "upload row must be gone after the purge");
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM jobs WHERE id = $1::uuid")
        .bind(job_id.clone())
        .fetch_one(&pool)
        .await
        .expect("count jobs after purge");
    assert_eq!(n, 0, "job row must be gone after the purge");
    let events: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM job_events je JOIN jobs j ON j.id = je.job_id WHERE j.user_id = $1::uuid",
    )
    .bind(user_id.clone())
    .fetch_one(&pool)
    .await
    .expect("count job events");
    assert_eq!(events, 0, "job events must be gone after the purge");
    let atts: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM attestations WHERE user_id = $1::uuid")
            .bind(user_id.clone())
            .fetch_one(&pool)
            .await
            .expect("count attestations");
    assert_eq!(atts, 0, "attestations must be gone after the purge");
    let actor: Option<String> = sqlx::query_scalar(
        "SELECT actor_user_id::text FROM audit_logs WHERE action = 'user.login' AND target_id = $1",
    )
    .bind(user_id.clone())
    .fetch_optional(&pool)
    .await
    .expect("read surviving audit row");
    assert_eq!(
        actor.as_deref(),
        Some(user_id.as_str()),
        "the audit row survives the purge with its actor intact"
    );

    // 5. Revert the purge migration: the FK comes back (NOT
    //    VALID — the surviving audit row names a deleted user)
    //    and the triggers go back to UPDATE OR DELETE; then
    //    re-apply restores the new contract.
    let reverted = migrate_revert_last(&pool, &mdir)
        .await
        .expect("revert purge migration");
    assert_eq!(reverted, Some(VERSION), "must revert the purge migration");
    assert_eq!(
        actor_fk_count(&pool).await,
        1,
        "revert restores the actor FK"
    );
    for trigger in ["job_events_append_only", "attestations_immutable"] {
        let def = trigger_def(&pool, trigger).await;
        assert!(
            def.contains("DELETE"),
            "{trigger} regains DELETE on revert: {def}"
        );
    }
    let reapplied = migrate_up(&pool, &mdir)
        .await
        .expect("re-apply purge migration");
    assert!(
        reapplied.contains(&VERSION),
        "purge migration must re-apply"
    );
    assert_eq!(
        actor_fk_count(&pool).await,
        0,
        "re-apply drops the actor FK again"
    );

    pool.close().await;
    drop_test_db(&url).await;
    let _ = std::fs::remove_dir_all(&mdir);
}
