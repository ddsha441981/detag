//! Live test for the P0-043 seed task (`just seed`).
//!
//! Runs `detag_core::seed::seed` against a scratch DATABASE
//! (`detag_p043_seed`, created and dropped by the test) with all
//! migrations applied, and asserts the acceptance criteria:
//! seed idempotent (run twice ok) — the second run changes
//! nothing, and an admin's edit survives a re-seed (ON CONFLICT
//! DO NOTHING policy, scripts/seed.sql header).
//!
//! Requires native Postgres on PF_DATABASE_URL (dev default
//! `postgres://pf:pf@localhost:5432/pf`; see DOCKER-001 —
//! containers cannot start on this VM, so the dev DB is native).
//! Needs CREATEDB for the `pf` role.

use detag_core::db::{connect, DbPool, PoolConfig};
use detag_core::migrate::migrate_up;
use detag_core::seed::seed;
use std::path::PathBuf;

const TEST_DB: &str = "detag_p043_seed";

fn database_url() -> String {
    std::env::var("PF_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://pf:pf@localhost:5432/pf".to_string())
}

fn test_database_url() -> String {
    let mut url = url::Url::parse(&database_url()).expect("PF_DATABASE_URL parses");
    url.set_path(&format!("/{TEST_DB}"));
    url.to_string()
}

fn migrations_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("migrations")
}

async fn setup() -> DbPool {
    let url = database_url();
    let admin = connect(&url, &PoolConfig::default())
        .await
        .unwrap_or_else(|err| {
            panic!(
                "live P0-043 test needs Postgres on {url}: {err} — start it with `just dev` (or the native postgres, see DOCKER-001)"
            )
        });
    sqlx::query("DROP DATABASE IF EXISTS detag_p043_seed")
        .execute(&admin)
        .await
        .expect("drop stale test db");
    sqlx::query("CREATE DATABASE detag_p043_seed")
        .execute(&admin)
        .await
        .unwrap_or_else(|err| panic!("live P0-043 test needs CREATEDB for the pf role: {err}"));
    admin.close().await;

    let pool = connect(&test_database_url(), &PoolConfig::default())
        .await
        .expect("connect to the scratch database");
    migrate_up(&pool, &migrations_dir())
        .await
        .expect("migrate up");
    pool
}

async fn teardown(pool: DbPool) {
    pool.close().await;
    let admin = connect(&database_url(), &PoolConfig::default())
        .await
        .expect("reconnect for cleanup");
    sqlx::query("DROP DATABASE IF EXISTS detag_p043_seed")
        .execute(&admin)
        .await
        .expect("drop test db");
    admin.close().await;
}

async fn plan_amount(pool: &DbPool, code: &str) -> i32 {
    sqlx::query_scalar("SELECT amount FROM plans WHERE code = $1")
        .bind(code)
        .fetch_one(pool)
        .await
        .expect("plan row exists")
}

async fn plan_daily_images(pool: &DbPool, code: &str) -> String {
    sqlx::query_scalar("SELECT entitlements->>'daily_images' FROM plans WHERE code = $1")
        .bind(code)
        .fetch_one(pool)
        .await
        .expect("plan row exists")
}

async fn flag_enabled(pool: &DbPool, key: &str) -> bool {
    sqlx::query_scalar("SELECT enabled FROM feature_flags WHERE key = $1")
        .bind(key)
        .fetch_one(pool)
        .await
        .expect("flag row exists")
}

#[tokio::test]
async fn seed_is_idempotent_and_never_clobbers_admin_edits() {
    let pool = setup().await;

    // First run: 3 plans (FR-P-01) + 3 flags, all OFF.
    let first = seed(&pool).await.expect("seed runs");
    assert_eq!(first.plans, 3, "3 plans seeded");
    assert_eq!(first.flags, 3, "3 flags seeded");
    assert_eq!(plan_amount(&pool, "free").await, 0);
    assert_eq!(plan_amount(&pool, "pro").await, 49900);
    assert_eq!(plan_amount(&pool, "business").await, 199900);
    assert_eq!(plan_daily_images(&pool, "free").await, "10");
    assert_eq!(plan_daily_images(&pool, "pro").await, "500");
    assert_eq!(plan_daily_images(&pool, "business").await, "5000");
    for key in ["razorpay", "provenance_strip", "gpu"] {
        assert!(!flag_enabled(&pool, key).await, "{key} must seed OFF");
    }

    // Second run: same counts, no duplicates, nothing changes.
    let second = seed(&pool).await.expect("seed runs twice");
    assert_eq!(second, first, "second run must report the same counts");

    // An admin edit survives a re-seed (ON CONFLICT DO NOTHING).
    sqlx::query(
        "UPDATE plans SET entitlements = jsonb_set(entitlements, '{daily_images}', '999') WHERE code = 'pro'",
    )
    .execute(&pool)
    .await
    .expect("admin edits a plan");
    sqlx::query(
        "UPDATE feature_flags SET enabled = TRUE, updated_by = 'admin' WHERE key = 'razorpay'",
    )
    .execute(&pool)
    .await
    .expect("admin flips a flag");
    let third = seed(&pool).await.expect("seed runs after edits");
    assert_eq!(third, first, "third run must still report the same counts");
    assert_eq!(
        plan_daily_images(&pool, "pro").await,
        "999",
        "admin plan edit survives"
    );
    assert!(
        flag_enabled(&pool, "razorpay").await,
        "admin flag edit survives"
    );

    teardown(pool).await;
}
