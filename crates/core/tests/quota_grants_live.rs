//! Live quota-grants tests (P1-107, FR-C-02).
//!
//! The repo behind the manual quota grant: one row per grant,
//! unexpired rows summed into the daily limits. These tests
//! run against the real Postgres: a scratch DB per test
//! (created + dropped by the test, the `repos_live.rs`
//! pattern — never the dev database). Rows are per fresh
//! user UUID, so parallel tests never share a grant.
//!
//! - `insert_roundtrip_and_unexpired_totals` — insert returns
//!   the grant with its fields; two live grants sum (5 + 3
//!   images); an already-expired grant contributes nothing.
//! - `expired_grants_contribute_nothing` — a grant's totals
//!   are (5, 0) before its expiry and (0, 0) after it; a
//!   video-only grant contributes only video-seconds.

use chrono::{Duration, TimeZone, Utc};
use detag_core::db::{connect, DbPool, PoolConfig};
use detag_core::migrate::migrate_up;
use detag_core::repos::{
    quota_grants::{self, NewQuotaGrant},
    users,
};
use std::path::PathBuf;
use uuid::Uuid;

fn database_url() -> String {
    std::env::var("PF_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://pf:pf@localhost:5432/pf".to_string())
}

fn test_database_url(db: &str) -> String {
    let mut url = url::Url::parse(&database_url()).expect("PF_DATABASE_URL parses");
    url.set_path(&format!("/{db}"));
    url.to_string()
}

fn migrations_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("migrations")
}

/// DDL cannot take bind parameters, and sqlx 0.9 rejects
/// dynamically built SQL strings — so the DROP/CREATE
/// statements are literals selected by the (known, fixed)
/// test DB name.
fn drop_db_sql(db: &str) -> &'static str {
    match db {
        "detag_p107_grants_totals" => "DROP DATABASE IF EXISTS detag_p107_grants_totals",
        "detag_p107_grants_expiry" => "DROP DATABASE IF EXISTS detag_p107_grants_expiry",
        _ => panic!("unknown P1-107 test db {db}"),
    }
}

fn create_db_sql(db: &str) -> &'static str {
    match db {
        "detag_p107_grants_totals" => "CREATE DATABASE detag_p107_grants_totals",
        "detag_p107_grants_expiry" => "CREATE DATABASE detag_p107_grants_expiry",
        _ => panic!("unknown P1-107 test db {db}"),
    }
}

async fn setup(db: &str) -> DbPool {
    let url = database_url();
    let admin = connect(&url, &PoolConfig::default())
        .await
        .unwrap_or_else(|err| {
            panic!(
                "live P1-107 test needs Postgres on {url}: {err} — start it with `just dev` (or the native postgres, see DOCKER-001)"
            )
        });
    sqlx::query(drop_db_sql(db))
        .execute(&admin)
        .await
        .expect("drop stale test db");
    sqlx::query(create_db_sql(db))
        .execute(&admin)
        .await
        .unwrap_or_else(|err| panic!("live P1-107 test needs CREATEDB for the pf role: {err}"));
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

async fn seed_user(pool: &DbPool) -> Uuid {
    let email = format!("grant-{}@example.com", Uuid::now_v7());
    users::create(pool, &email, "argon2id$placeholder")
        .await
        .expect("seed user")
        .id
}

#[tokio::test]
async fn insert_roundtrip_and_unexpired_totals() {
    let db = "detag_p107_grants_totals";
    let pool = setup(db).await;
    let user_id = seed_user(&pool).await;
    let admin_id = seed_user(&pool).await;
    let now = Utc.with_ymd_and_hms(2026, 11, 19, 12, 0, 0).unwrap();

    // Insert returns the grant with its fields.
    let grant = quota_grants::insert(
        &pool,
        &NewQuotaGrant {
            user_id,
            images: Some(5),
            video_seconds: Some(300),
            expires_at: now + Duration::hours(24),
            reason: "goodwill".to_string(),
            granted_by: admin_id,
        },
    )
    .await
    .expect("insert grant");
    assert_eq!(grant.user_id, user_id);
    assert_eq!(grant.images, Some(5));
    assert_eq!(grant.video_seconds, Some(300));
    assert_eq!(grant.expires_at, now + Duration::hours(24));
    assert_eq!(grant.reason, "goodwill");
    assert_eq!(grant.granted_by, admin_id);
    assert_eq!(
        quota_grants::unexpired_totals(&pool, user_id, now)
            .await
            .expect("totals"),
        (5, 300),
        "one live grant counts"
    );

    // A second live grant stacks: 5 + 3 images.
    quota_grants::insert(
        &pool,
        &NewQuotaGrant {
            user_id,
            images: Some(3),
            video_seconds: None,
            expires_at: now + Duration::hours(48),
            reason: "stack".to_string(),
            granted_by: admin_id,
        },
    )
    .await
    .expect("insert second grant");
    assert_eq!(
        quota_grants::unexpired_totals(&pool, user_id, now)
            .await
            .expect("totals"),
        (8, 300),
        "grants stack"
    );

    // An already-expired grant contributes nothing.
    quota_grants::insert(
        &pool,
        &NewQuotaGrant {
            user_id,
            images: Some(100),
            video_seconds: Some(1000),
            expires_at: now - Duration::hours(1),
            reason: "expired".to_string(),
            granted_by: admin_id,
        },
    )
    .await
    .expect("insert expired grant");
    assert_eq!(
        quota_grants::unexpired_totals(&pool, user_id, now)
            .await
            .expect("totals"),
        (8, 300),
        "an expired grant is history, not quota"
    );

    teardown(pool, db).await;
}

#[tokio::test]
async fn expired_grants_contribute_nothing() {
    let db = "detag_p107_grants_expiry";
    let pool = setup(db).await;
    let user_id = seed_user(&pool).await;
    let admin_id = seed_user(&pool).await;
    let now = Utc.with_ymd_and_hms(2026, 11, 19, 12, 0, 0).unwrap();

    quota_grants::insert(
        &pool,
        &NewQuotaGrant {
            user_id,
            images: Some(5),
            video_seconds: None,
            expires_at: now + Duration::hours(1),
            reason: "short".to_string(),
            granted_by: admin_id,
        },
    )
    .await
    .expect("insert grant");
    assert_eq!(
        quota_grants::unexpired_totals(&pool, user_id, now)
            .await
            .expect("totals"),
        (5, 0),
        "live before expiry"
    );
    assert_eq!(
        quota_grants::unexpired_totals(&pool, user_id, now + Duration::hours(2))
            .await
            .expect("totals"),
        (0, 0),
        "dead after expiry"
    );

    // A video-only grant contributes only video-seconds.
    quota_grants::insert(
        &pool,
        &NewQuotaGrant {
            user_id,
            images: None,
            video_seconds: Some(300),
            expires_at: now + Duration::hours(24),
            reason: "video only".to_string(),
            granted_by: admin_id,
        },
    )
    .await
    .expect("insert video grant");
    assert_eq!(
        quota_grants::unexpired_totals(&pool, user_id, now)
            .await
            .expect("totals"),
        (5, 300),
        "video-only adds no images"
    );

    teardown(pool, db).await;
}
