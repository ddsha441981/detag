//! Live test for migration 003 — billing (P0-039, E0.4).
//!
//! Applies ONLY the 001 + 003 migration pairs (copied from the
//! repo's real `migrations/` into a scratch dir — later migrations
//! must not change what this test applies or reverts; 003 depends
//! on 001's users table, not on 002) to a scratch DATABASE
//! (`detag_p039_test`, created and dropped by the test) — never the
//! dev database. Asserts the plan seed matches FR-P-01, the billing
//! CHECKs, the usage_counters PK, plus the migration's revert, then
//! cleans up. The `pf` role needs CREATEDB (granted in the VM dev
//! setup; CI's postgres service user has it).

use detag_core::db::{connect, DbPool, PoolConfig};
use detag_core::migrate::{migrate_revert_last, migrate_up};
use std::path::PathBuf;

const TEST_DB: &str = "detag_p039_test";
const VERSION: i64 = 20261112065140;

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
    let dir = std::env::temp_dir().join(format!("detag-p039-migrations-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch migrations dir");
    let repo = repo_migrations_dir();
    for stem in ["20261112064220_users_sessions", "20261112065140_billing"] {
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

async fn create_test_db(admin_url: &str) {
    let admin = connect(admin_url, &PoolConfig::default())
        .await
        .unwrap_or_else(|err| {
            panic!(
                "live P0-039 test needs Postgres on {admin_url}: {err} — start it with `just dev` (or the native postgres, see DOCKER-001)"
            )
        });
    sqlx::query("DROP DATABASE IF EXISTS detag_p039_test")
        .execute(&admin)
        .await
        .expect("drop stale test db");
    sqlx::query("CREATE DATABASE detag_p039_test")
        .execute(&admin)
        .await
        .unwrap_or_else(|err| panic!("live P0-039 test needs CREATEDB for the pf role: {err}"));
    admin.close().await;
}

async fn drop_test_db(admin_url: &str) {
    let admin = connect(admin_url, &PoolConfig::default())
        .await
        .expect("reconnect for cleanup");
    sqlx::query("DROP DATABASE IF EXISTS detag_p039_test")
        .execute(&admin)
        .await
        .expect("drop test db");
    admin.close().await;
}

/// The seed INSERTs from the 003 up migration, verbatim: re-running
/// them must be a no-op (ON CONFLICT DO NOTHING).
const SEED_SQL: &str = "INSERT INTO plans (code, currency, amount, entitlements) VALUES
    ('free', 'INR', 0, '{\"daily_images\": 10, \"daily_video_seconds\": 0, \"max_bytes\": 26214400, \"result_ttl_hours\": 6, \"concurrency\": 1, \"lane\": \"standard\", \"features\": {}}'::jsonb),
    ('pro', 'INR', 49900, '{\"daily_images\": 500, \"daily_video_seconds\": 3600, \"max_bytes\": 1073741824, \"result_ttl_hours\": 24, \"concurrency\": 3, \"lane\": \"standard\", \"features\": {\"api_keys\": true, \"webhooks\": true}}'::jsonb),
    ('business', 'INR', 199900, '{\"daily_images\": 5000, \"daily_video_seconds\": 18000, \"max_bytes\": 4294967296, \"result_ttl_hours\": 72, \"concurrency\": 10, \"lane\": \"priority\", \"features\": {\"api_keys\": true, \"webhooks\": true, \"batch\": true}}'::jsonb)
ON CONFLICT (code) DO NOTHING";

#[tokio::test]
async fn migration_003_billing_seed_and_constraints() {
    let url = database_url();
    create_test_db(&url).await;
    let pool = connect(&test_database_url(), &PoolConfig::default())
        .await
        .expect("connect to the scratch database");
    let mdir = migrations_dir();

    let applied = migrate_up(&pool, &mdir).await.expect("migrate up");
    assert!(
        applied.contains(&VERSION),
        "migration 003 must apply: {applied:?}"
    );
    for table in ["plans", "subscriptions", "usage_counters"] {
        assert!(table_exists(&pool, table).await, "{table} must exist");
    }

    // Seed rows: exactly Free/Pro/Business, entitlements per FR-P-01.
    let plan_count: i64 = sqlx::query_scalar("SELECT count(*) FROM plans")
        .fetch_one(&pool)
        .await
        .expect("count plans");
    assert_eq!(plan_count, 3, "exactly three seeded plans");
    for (code, images, video_secs, max_bytes, ttl, concurrency, lane) in [
        ("free", "10", "0", "26214400", "6", "1", "standard"),
        ("pro", "500", "3600", "1073741824", "24", "3", "standard"),
        (
            "business",
            "5000",
            "18000",
            "4294967296",
            "72",
            "10",
            "priority",
        ),
    ] {
        let row: (String, String, String, String, String, String) = sqlx::query_as(
            "SELECT entitlements->>'daily_images', entitlements->>'daily_video_seconds', \
             entitlements->>'max_bytes', entitlements->>'result_ttl_hours', \
             entitlements->>'concurrency', entitlements->>'lane' \
             FROM plans WHERE code = $1",
        )
        .bind(code)
        .fetch_one(&pool)
        .await
        .unwrap_or_else(|err| panic!("plan {code} must exist: {err}"));
        assert_eq!(
            row,
            (
                images.to_string(),
                video_secs.to_string(),
                max_bytes.to_string(),
                ttl.to_string(),
                concurrency.to_string(),
                lane.to_string()
            ),
            "plan {code} entitlements must match FR-P-01"
        );
    }
    let pro_features: String =
        sqlx::query_scalar("SELECT entitlements->>'features' FROM plans WHERE code = 'pro'")
            .fetch_one(&pool)
            .await
            .expect("pro features");
    assert!(
        pro_features.contains("api_keys") && pro_features.contains("webhooks"),
        "Pro must have API keys + webhooks: {pro_features}"
    );
    let biz_features: String =
        sqlx::query_scalar("SELECT entitlements->>'features' FROM plans WHERE code = 'business'")
            .fetch_one(&pool)
            .await
            .expect("business features");
    assert!(
        biz_features.contains("batch"),
        "Business must have batch: {biz_features}"
    );

    // Seed idempotency: re-running the INSERTs changes nothing.
    sqlx::query(SEED_SQL)
        .execute(&pool)
        .await
        .expect("re-run seed inserts");
    let plan_count: i64 = sqlx::query_scalar("SELECT count(*) FROM plans")
        .fetch_one(&pool)
        .await
        .expect("count plans again");
    assert_eq!(plan_count, 3, "seed re-run must not add rows");

    let user_id: String = sqlx::query_scalar(
        "INSERT INTO users (email, password_hash) \
         VALUES ('p039@example.com', 'argon2id$placeholder') RETURNING id::text",
    )
    .fetch_one(&pool)
    .await
    .expect("insert user");
    let plan_id: String = sqlx::query_scalar("SELECT id::text FROM plans WHERE code = 'pro'")
        .fetch_one(&pool)
        .await
        .expect("pro plan id");

    // subscriptions provider/status CHECKs.
    let bad_provider = sqlx::query(
        "INSERT INTO subscriptions (user_id, plan_id, provider, status) \
         VALUES ($1::uuid, $2::uuid, 'paypal', 'active')",
    )
    .bind(user_id.clone())
    .bind(plan_id.clone())
    .execute(&pool)
    .await;
    assert!(
        bad_provider.is_err(),
        "provider outside stripe|razorpay must fail"
    );
    let bad_status = sqlx::query(
        "INSERT INTO subscriptions (user_id, plan_id, provider, status) \
         VALUES ($1::uuid, $2::uuid, 'stripe', 'expired')",
    )
    .bind(user_id.clone())
    .bind(plan_id.clone())
    .execute(&pool)
    .await;
    assert!(bad_status.is_err(), "status outside the doc list must fail");
    sqlx::query(
        "INSERT INTO subscriptions (user_id, plan_id, provider, status) \
         VALUES ($1::uuid, $2::uuid, 'stripe', 'active')",
    )
    .bind(user_id.clone())
    .bind(plan_id.clone())
    .execute(&pool)
    .await
    .expect("insert subscription");

    // A plan with subscribers cannot be deleted (RESTRICT).
    let delete_plan = sqlx::query("DELETE FROM plans WHERE id = $1::uuid")
        .bind(plan_id.clone())
        .execute(&pool)
        .await;
    assert!(delete_plan.is_err(), "deleting a subscribed plan must fail");

    // usage_counters PK (user_id, day): duplicate fails; negatives fail.
    sqlx::query(
        "INSERT INTO usage_counters (user_id, day, images_used, video_seconds_used) \
         VALUES ($1::uuid, CURRENT_DATE, 3, 120)",
    )
    .bind(user_id.clone())
    .execute(&pool)
    .await
    .expect("insert usage counter");
    let dup_day = sqlx::query(
        "INSERT INTO usage_counters (user_id, day, images_used) \
         VALUES ($1::uuid, CURRENT_DATE, 1)",
    )
    .bind(user_id.clone())
    .execute(&pool)
    .await;
    assert!(
        dup_day.is_err(),
        "second counter for the same user+day must fail"
    );
    let negative = sqlx::query(
        "INSERT INTO usage_counters (user_id, day, images_used) \
         VALUES ($1::uuid, CURRENT_DATE - 1, -1)",
    )
    .bind(user_id.clone())
    .execute(&pool)
    .await;
    assert!(negative.is_err(), "negative counter must fail");

    // Revert 003: all three tables go away; re-apply restores them
    // and reseeds the three plans.
    let reverted = migrate_revert_last(&pool, &mdir).await.expect("revert 003");
    assert_eq!(reverted, Some(VERSION), "must revert 003");
    for table in ["plans", "subscriptions", "usage_counters"] {
        assert!(!table_exists(&pool, table).await, "revert drops {table}");
    }
    let reapplied = migrate_up(&pool, &mdir).await.expect("re-apply 003");
    assert!(
        reapplied.contains(&VERSION),
        "re-apply must restore 003: {reapplied:?}"
    );
    let plan_count: i64 = sqlx::query_scalar("SELECT count(*) FROM plans")
        .fetch_one(&pool)
        .await
        .expect("count plans after re-apply");
    assert_eq!(plan_count, 3, "re-apply must reseed the three plans");

    pool.close().await;
    drop_test_db(&url).await;
    let _ = std::fs::remove_dir_all(&mdir);
}
