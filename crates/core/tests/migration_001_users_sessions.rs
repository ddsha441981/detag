//! Live test for migration 001 — users + sessions (P0-037, E0.4).
//!
//! Applies ONLY the 001 migration pair (copied from the repo's real
//! `migrations/` into a scratch dir — later migrations must not
//! change what this test applies or reverts) to a scratch DATABASE
//! (`detag_p037_test`, created and dropped by the test) — never the
//! dev database, so the dev `_sqlx_migrations` is never touched
//! (sqlx fails a later `just migrate` when applied versions are
//! missing from the source, `VersionMissing`). Asserts the schema
//! from docs/03-ARCHITECTURE/04-data-model.md (types, constraints,
//! unique citext email) plus the migration's revert, then cleans up.
//! The `pf` role needs CREATEDB (granted in the VM dev setup; CI's
//! postgres service user has it).

use detag_core::db::{connect, DbPool, PoolConfig};
use detag_core::migrate::{migrate_revert_last, migrate_up};
use std::path::PathBuf;

const TEST_DB: &str = "detag_p037_test";

fn database_url() -> String {
    std::env::var("PF_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://pf:pf@localhost:5432/pf".to_string())
}

fn test_database_url() -> String {
    let mut url = url::Url::parse(&database_url()).expect("PF_DATABASE_URL parses");
    url.set_path(&format!("/{TEST_DB}"));
    url.to_string()
}

/// The repo's real migrations dir (this file lives in
/// `crates/core/tests/`, so two levels up is the workspace root).
fn repo_migrations_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("migrations")
}

/// A scratch dir holding ONLY the 001 pair, copied from the repo.
/// Later migrations (002, 003, ...) must not change what this test
/// applies or reverts, so the test never runs the whole `migrations/`
/// dir: `migrate_revert_last` reverts the last applied version, and
/// that must be 001 here.
fn migrations_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("detag-p037-migrations-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch migrations dir");
    let repo = repo_migrations_dir();
    for ext in ["up.sql", "down.sql"] {
        let name = format!("20261112064220_users_sessions.{ext}");
        std::fs::copy(repo.join(&name), dir.join(&name)).expect("copy 001 migration pair");
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

async fn create_test_db(admin_url: &str) {
    let admin = connect(admin_url, &PoolConfig::default())
        .await
        .unwrap_or_else(|err| {
            panic!(
                "live P0-037 test needs Postgres on {admin_url}: {err} — start it with `just dev` (or the native postgres, see DOCKER-001)"
            )
        });
    sqlx::query("DROP DATABASE IF EXISTS detag_p037_test")
        .execute(&admin)
        .await
        .expect("drop stale test db");
    sqlx::query("CREATE DATABASE detag_p037_test")
        .execute(&admin)
        .await
        .unwrap_or_else(|err| panic!("live P0-037 test needs CREATEDB for the pf role: {err}"));
    admin.close().await;
}

async fn drop_test_db(admin_url: &str) {
    let admin = connect(admin_url, &PoolConfig::default())
        .await
        .expect("reconnect for cleanup");
    sqlx::query("DROP DATABASE IF EXISTS detag_p037_test")
        .execute(&admin)
        .await
        .expect("drop test db");
    admin.close().await;
}

#[tokio::test]
async fn migration_001_users_sessions_schema_and_constraints() {
    let url = database_url();
    create_test_db(&url).await;
    let pool = connect(&test_database_url(), &PoolConfig::default())
        .await
        .expect("connect to the scratch database");
    let mdir = migrations_dir();

    // Apply the 001 pair only (see migrations_dir()).
    let applied = migrate_up(&pool, &mdir).await.expect("migrate up");
    assert!(
        applied.contains(&20261112064220),
        "migration 001 must apply: {applied:?}"
    );
    assert!(table_exists(&pool, "users").await, "users must exist");
    assert!(table_exists(&pool, "sessions").await, "sessions must exist");

    // Email is citext, unique, case-insensitive.
    let email_type: String = sqlx::query_scalar(
        "SELECT data_type FROM information_schema.columns \
         WHERE table_name = 'users' AND column_name = 'email'",
    )
    .fetch_one(&pool)
    .await
    .expect("email column type");
    // information_schema reports citext as USER-DEFINED; check the
    // udt name instead.
    let email_udt: String = sqlx::query_scalar(
        "SELECT udt_name FROM information_schema.columns \
         WHERE table_name = 'users' AND column_name = 'email'",
    )
    .fetch_one(&pool)
    .await
    .expect("email column udt");
    assert_eq!(email_type, "USER-DEFINED", "email must be a custom type");
    assert_eq!(email_udt, "citext", "email must be citext, got {email_udt}");

    let user_id: String = sqlx::query_scalar(
        "INSERT INTO users (email, password_hash) \
         VALUES ('Alice@Example.com', 'argon2id$placeholder') RETURNING id::text",
    )
    .fetch_one(&pool)
    .await
    .expect("insert first user");
    let dup =
        sqlx::query("INSERT INTO users (email, password_hash) VALUES ('alice@example.com', 'x')")
            .execute(&pool)
            .await;
    assert!(dup.is_err(), "same email, different case, must be rejected");
    let err = dup.expect_err("duplicate insert must fail");
    assert!(
        err.to_string().contains("users_email_key"),
        "must fail on the email unique constraint, got {err}"
    );

    // role / status CHECK constraints reject values outside the doc.
    let bad_role = sqlx::query(
        "INSERT INTO users (email, password_hash, role) VALUES ('b@example.com', 'x', 'superuser')",
    )
    .execute(&pool)
    .await;
    assert!(
        bad_role.is_err(),
        "role outside user|support|admin must fail"
    );
    let bad_status = sqlx::query(
        "INSERT INTO users (email, password_hash, status) VALUES ('c@example.com', 'x', 'banned')",
    )
    .execute(&pool)
    .await;
    assert!(
        bad_status.is_err(),
        "status outside active|suspended|deletion_pending must fail"
    );

    // Sessions: FK to users, unique refresh_hash, device jsonb.
    let bad_fk = sqlx::query(
        "INSERT INTO sessions (user_id, refresh_hash, family_id, expires_at) \
         VALUES ('00000000-0000-0000-0000-000000000000', 'h', gen_random_uuid(), now() + interval '1 day')",
    )
    .execute(&pool)
    .await;
    assert!(bad_fk.is_err(), "session for an unknown user must fail");

    sqlx::query(
        "INSERT INTO sessions (user_id, refresh_hash, family_id, device, expires_at) \
         VALUES ($1::uuid, 'sha256-placeholder', gen_random_uuid(), '{\"ua\": \"test\", \"ip_hash\": \"abc\"}', now() + interval '1 day')",
    )
    .bind(user_id.clone())
    .execute(&pool)
    .await
    .expect("insert session");
    let dup_hash = sqlx::query(
        "INSERT INTO sessions (user_id, refresh_hash, family_id, expires_at) \
         VALUES ($1::uuid, 'sha256-placeholder', gen_random_uuid(), now() + interval '1 day')",
    )
    .bind(user_id)
    .execute(&pool)
    .await;
    assert!(dup_hash.is_err(), "duplicate refresh_hash must fail");

    // Revert 001: both tables go away; re-apply brings them back.
    let reverted = migrate_revert_last(&pool, &mdir).await.expect("revert 001");
    assert_eq!(reverted, Some(20261112064220), "must revert 001");
    assert!(!table_exists(&pool, "users").await, "revert drops users");
    assert!(
        !table_exists(&pool, "sessions").await,
        "revert drops sessions"
    );
    let reapplied = migrate_up(&pool, &mdir).await.expect("re-apply 001");
    assert!(
        reapplied.contains(&20261112064220),
        "re-apply must restore 001: {reapplied:?}"
    );

    pool.close().await;
    drop_test_db(&url).await;
    let _ = std::fs::remove_dir_all(&mdir);
}
