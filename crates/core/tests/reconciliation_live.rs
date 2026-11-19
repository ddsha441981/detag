//! Live nightly-reconciliation tests (P1-082, FR-P-07).
//!
//! The job compares the Redis day-hash against the durable
//! `usage_counters` row for every user-day in the window
//! (today + yesterday), the DB wins, and corrections beyond
//! the audit thresholds land one `billing.usage.reconciled`
//! audit row each. These tests run against the real
//! services: a scratch Postgres DB per test (created +
//! dropped by the test, the `usage_live.rs` pattern) and the
//! dev Redis (`PF_REDIS_URL`, dev default
//! `redis://localhost:6379`).
//!
//! Isolation note — the job enumerates candidates with a
//! Redis SCAN, so parallel tests must never share a window
//! day: each test freezes its clock on a distinct March-2027
//! date, spaced 3 days apart, so no two windows overlap and
//! no run can reconcile (or zero) another test's hash. No
//! other suite freezes a 2027 date, so the SCAN patterns
//! match only this file's own keys.
//!
//! - `drift_corrected_and_audited` — the ticket's AC: a
//!   yesterday drift beyond the video threshold is
//!   corrected in Redis and audited.
//! - `small_drift_corrected_without_audit` — drift under
//!   both thresholds is corrected but NOT audited.
//! - `clean_day_is_a_no_op` — a day that already agrees is
//!   checked and left alone.
//! - `redis_only_drift_is_zeroed_db_wins` — a Redis hash
//!   with no DB row is zeroed (DB wins, ASSUMPTION-043).
//! - `disabled_job_does_not_run` — the kill-switch leaves
//!   Redis untouched and reports `ran: false`.
//! - `one_bad_day_does_not_abort_the_sweep` — a day whose
//!   Redis key is a plain string (not a hash) errors and is
//!   counted, and the other day is still corrected.
//! - `old_drift_is_outside_the_window` — drift three days
//!   back is untouched: the window is today + yesterday.

use chrono::{NaiveDate, TimeZone, Utc};
use deadpool_redis::redis::cmd;
use detag_core::clock::ManualClock;
use detag_core::db::{connect, DbPool, PoolConfig};
use detag_core::migrate::migrate_up;
use detag_core::reconciliation::{run_reconciliation, ReconcileConfig, RECONCILE_AUDIT_ACTION};
use detag_core::redis::keys::usage_key;
use detag_core::redis::{create_pool, RedisPool, RedisPoolConfig};
use detag_core::repos::{audit_logs, usage_counters, users};
use std::path::PathBuf;
use std::sync::Arc;
use uuid::Uuid;

fn database_url() -> String {
    std::env::var("PF_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://pf:pf@localhost:5432/pf".to_string())
}

fn redis_url() -> String {
    std::env::var("PF_REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".to_string())
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
        "detag_p082_recon_ac" => "DROP DATABASE IF EXISTS detag_p082_recon_ac",
        "detag_p082_recon_small" => "DROP DATABASE IF EXISTS detag_p082_recon_small",
        "detag_p082_recon_clean" => "DROP DATABASE IF EXISTS detag_p082_recon_clean",
        "detag_p082_recon_redis_only" => "DROP DATABASE IF EXISTS detag_p082_recon_redis_only",
        "detag_p082_recon_disabled" => "DROP DATABASE IF EXISTS detag_p082_recon_disabled",
        "detag_p082_recon_bad_day" => "DROP DATABASE IF EXISTS detag_p082_recon_bad_day",
        "detag_p082_recon_old_day" => "DROP DATABASE IF EXISTS detag_p082_recon_old_day",
        _ => panic!("unknown P1-082 test db {db}"),
    }
}

fn create_db_sql(db: &str) -> &'static str {
    match db {
        "detag_p082_recon_ac" => "CREATE DATABASE detag_p082_recon_ac",
        "detag_p082_recon_small" => "CREATE DATABASE detag_p082_recon_small",
        "detag_p082_recon_clean" => "CREATE DATABASE detag_p082_recon_clean",
        "detag_p082_recon_redis_only" => "CREATE DATABASE detag_p082_recon_redis_only",
        "detag_p082_recon_disabled" => "CREATE DATABASE detag_p082_recon_disabled",
        "detag_p082_recon_bad_day" => "CREATE DATABASE detag_p082_recon_bad_day",
        "detag_p082_recon_old_day" => "CREATE DATABASE detag_p082_recon_old_day",
        _ => panic!("unknown P1-082 test db {db}"),
    }
}

async fn setup(db: &str) -> DbPool {
    let url = database_url();
    let admin = connect(&url, &PoolConfig::default())
        .await
        .unwrap_or_else(|err| {
            panic!(
                "live P1-082 test needs Postgres on {url}: {err} — start it with `just dev` (or the native postgres, see DOCKER-001)"
            )
        });
    sqlx::query(drop_db_sql(db))
        .execute(&admin)
        .await
        .expect("drop stale test db");
    sqlx::query(create_db_sql(db))
        .execute(&admin)
        .await
        .unwrap_or_else(|err| panic!("live P1-082 test needs CREATEDB for the pf role: {err}"));
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

fn redis_pool() -> RedisPool {
    create_pool(&redis_url(), &RedisPoolConfig::default()).expect("pool builds from the dev URL")
}

async fn seed_user(pool: &DbPool) -> Uuid {
    let email = format!("recon-{}@example.com", Uuid::now_v7());
    users::create(pool, &email, "argon2id$placeholder")
        .await
        .expect("seed user")
        .id
}

/// The Redis hash for a user-day, as (images, video_seconds)
/// — absent fields read as 0.
async fn redis_totals(pool: &RedisPool, user_id: Uuid, day: NaiveDate) -> (i64, i64) {
    let mut conn = pool.get().await.expect("checkout");
    let got: (Option<i64>, Option<i64>) = cmd("HMGET")
        .arg(usage_key(&user_id, day))
        .arg("images")
        .arg("video_seconds")
        .query_async(&mut conn)
        .await
        .expect("HMGET the day hash");
    (got.0.unwrap_or(0), got.1.unwrap_or(0))
}

/// Set a user-day's Redis hash behind the meter's back
/// (the drift under test).
async fn set_redis(pool: &RedisPool, user_id: Uuid, day: NaiveDate, images: i64, video: i64) {
    let mut conn = pool.get().await.expect("checkout");
    let _: () = cmd("HSET")
        .arg(usage_key(&user_id, day))
        .arg("images")
        .arg(images)
        .arg("video_seconds")
        .arg(video)
        .query_async(&mut conn)
        .await
        .expect("HSET the day hash");
}

async fn delete_key(pool: &RedisPool, user_id: Uuid, day: NaiveDate) {
    let mut conn = pool.get().await.expect("checkout");
    let _: () = cmd("DEL")
        .arg(usage_key(&user_id, day))
        .query_async(&mut conn)
        .await
        .expect("DEL the day hash");
}

/// Drop every usage hash for `day` — the start-of-test
/// cleanup: a VM reset mid-test leaves the previous run's
/// hashes behind (different user UUIDs, same window days),
/// and the job's SCAN would find them, DB-miss them, and
/// count them as checked. A crashed run must not break the
/// next run's counts.
async fn delete_day_keys(pool: &RedisPool, day: NaiveDate) {
    let pattern = format!("pf:usage:*:{day}", day = day.format("%Y-%m-%d"));
    let mut cursor = "0".to_string();
    loop {
        let mut conn = pool.get().await.expect("checkout");
        let (next, keys): (String, Vec<String>) = cmd("SCAN")
            .arg(&cursor)
            .arg("MATCH")
            .arg(&pattern)
            .arg("COUNT")
            .arg(1000)
            .query_async(&mut conn)
            .await
            .expect("SCAN the day pattern");
        if !keys.is_empty() {
            let _: () = cmd("DEL")
                .arg(keys)
                .query_async(&mut conn)
                .await
                .expect("DEL the stale day hashes");
        }
        cursor = next;
        if cursor == "0" {
            break;
        }
    }
}

fn config() -> ReconcileConfig {
    ReconcileConfig {
        enabled: true,
        audit_threshold_images: 10,
        audit_threshold_video_seconds: 60,
    }
}

/// The ticket's AC: drift beyond the threshold is corrected
/// in Redis AND audited.
#[tokio::test]
async fn drift_corrected_and_audited() {
    let db = "detag_p082_recon_ac";
    let pool = setup(db).await;
    let redis = redis_pool();
    let user_id = seed_user(&pool).await;
    // Frozen at 02:00 UTC on 2027-03-10 (the nightly hour):
    // the window is 03-09 + 03-10.
    let clock = ManualClock::new(Utc.with_ymd_and_hms(2027, 3, 10, 2, 0, 0).unwrap());
    delete_day_keys(&redis, NaiveDate::from_ymd_opt(2027, 3, 9).unwrap()).await;
    delete_day_keys(&redis, NaiveDate::from_ymd_opt(2027, 3, 10).unwrap()).await;
    let day = NaiveDate::from_ymd_opt(2027, 3, 9).unwrap();
    usage_counters::increment(&pool, user_id, day, 5, 120)
        .await
        .expect("seed the DB row");
    set_redis(&redis, user_id, day, 8, 300).await;

    let report = run_reconciliation(&pool, &redis, Arc::new(clock), &config())
        .await
        .expect("the run succeeds");
    assert!(report.ran);
    assert_eq!(report.checked, 1);
    assert_eq!(report.corrected, 1);
    assert_eq!(report.audited, 1);
    assert_eq!(report.errors, 0);
    assert_eq!(report.anomalies.len(), 1);
    assert_eq!(redis_totals(&redis, user_id, day).await, (5, 120));

    let rows = audit_logs::list_for_action(&pool, RECONCILE_AUDIT_ACTION)
        .await
        .expect("read the audit rows");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].target_type, "user");
    assert_eq!(
        rows[0].target_id.as_deref(),
        Some(user_id.to_string()).as_deref()
    );
    assert_eq!(rows[0].actor_client.as_deref(), Some("reconciliation"));
    assert_eq!(rows[0].actor_user_id, None);
    let before = rows[0].before.as_ref().expect("before payload");
    assert_eq!(before["images"], 8);
    assert_eq!(before["video_seconds"], 300);
    let after = rows[0].after.as_ref().expect("after payload");
    assert_eq!(after["images"], 5);
    assert_eq!(after["video_seconds"], 120);

    delete_key(&redis, user_id, day).await;
    teardown(pool, db).await;
}

/// Drift under both thresholds is corrected but NOT
/// audited — small drift is race/clock noise, not an
/// anomaly.
#[tokio::test]
async fn small_drift_corrected_without_audit() {
    let db = "detag_p082_recon_small";
    let pool = setup(db).await;
    let redis = redis_pool();
    let user_id = seed_user(&pool).await;
    let clock = ManualClock::new(Utc.with_ymd_and_hms(2027, 3, 13, 2, 0, 0).unwrap());
    delete_day_keys(&redis, NaiveDate::from_ymd_opt(2027, 3, 12).unwrap()).await;
    delete_day_keys(&redis, NaiveDate::from_ymd_opt(2027, 3, 13).unwrap()).await;
    let day = NaiveDate::from_ymd_opt(2027, 3, 12).unwrap();
    usage_counters::increment(&pool, user_id, day, 5, 120)
        .await
        .expect("seed the DB row");
    // Δimages 2 (< 10), Δvideo 10 (< 60): corrected, not audited.
    set_redis(&redis, user_id, day, 7, 130).await;

    let report = run_reconciliation(&pool, &redis, Arc::new(clock), &config())
        .await
        .expect("the run succeeds");
    assert!(report.ran);
    assert_eq!(report.checked, 1);
    assert_eq!(report.corrected, 1);
    assert_eq!(report.audited, 0);
    assert_eq!(report.errors, 0);
    assert!(report.anomalies.is_empty());
    assert_eq!(redis_totals(&redis, user_id, day).await, (5, 120));
    let rows = audit_logs::list_for_action(&pool, RECONCILE_AUDIT_ACTION)
        .await
        .expect("read the audit rows");
    assert!(rows.is_empty(), "no audit row for sub-threshold drift");

    delete_key(&redis, user_id, day).await;
    teardown(pool, db).await;
}

/// A day that already agrees is checked and left alone.
#[tokio::test]
async fn clean_day_is_a_no_op() {
    let db = "detag_p082_recon_clean";
    let pool = setup(db).await;
    let redis = redis_pool();
    let user_id = seed_user(&pool).await;
    let clock = ManualClock::new(Utc.with_ymd_and_hms(2027, 3, 16, 2, 0, 0).unwrap());
    delete_day_keys(&redis, NaiveDate::from_ymd_opt(2027, 3, 15).unwrap()).await;
    delete_day_keys(&redis, NaiveDate::from_ymd_opt(2027, 3, 16).unwrap()).await;
    let day = NaiveDate::from_ymd_opt(2027, 3, 15).unwrap();
    usage_counters::increment(&pool, user_id, day, 5, 120)
        .await
        .expect("seed the DB row");
    set_redis(&redis, user_id, day, 5, 120).await;

    let report = run_reconciliation(&pool, &redis, Arc::new(clock), &config())
        .await
        .expect("the run succeeds");
    assert!(report.ran);
    assert_eq!(report.checked, 1);
    assert_eq!(report.corrected, 0);
    assert_eq!(report.audited, 0);
    assert_eq!(report.errors, 0);
    assert_eq!(redis_totals(&redis, user_id, day).await, (5, 120));

    delete_key(&redis, user_id, day).await;
    teardown(pool, db).await;
}

/// A Redis hash with no DB row is zeroed: DB wins, and a
/// row the DB never got is not usage the quota decisions
/// can see (ASSUMPTION-043). The zeroing IS an anomaly
/// here — the video leg drops 90 seconds, past the 60
/// threshold — so it is audited too.
#[tokio::test]
async fn redis_only_drift_is_zeroed_db_wins() {
    let db = "detag_p082_recon_redis_only";
    let pool = setup(db).await;
    let redis = redis_pool();
    let user_id = seed_user(&pool).await;
    let clock = ManualClock::new(Utc.with_ymd_and_hms(2027, 3, 19, 2, 0, 0).unwrap());
    delete_day_keys(&redis, NaiveDate::from_ymd_opt(2027, 3, 18).unwrap()).await;
    delete_day_keys(&redis, NaiveDate::from_ymd_opt(2027, 3, 19).unwrap()).await;
    let day = NaiveDate::from_ymd_opt(2027, 3, 18).unwrap();
    set_redis(&redis, user_id, day, 4, 90).await;

    let report = run_reconciliation(&pool, &redis, Arc::new(clock), &config())
        .await
        .expect("the run succeeds");
    assert!(report.ran);
    assert_eq!(report.checked, 1);
    assert_eq!(report.corrected, 1);
    assert_eq!(report.audited, 1);
    assert_eq!(report.errors, 0);
    assert_eq!(redis_totals(&redis, user_id, day).await, (0, 0));

    delete_key(&redis, user_id, day).await;
    teardown(pool, db).await;
}

/// The kill-switch: a disabled job reports `ran: false`
/// and leaves Redis untouched.
#[tokio::test]
async fn disabled_job_does_not_run() {
    let db = "detag_p082_recon_disabled";
    let pool = setup(db).await;
    let redis = redis_pool();
    let user_id = seed_user(&pool).await;
    let clock = ManualClock::new(Utc.with_ymd_and_hms(2027, 3, 22, 2, 0, 0).unwrap());
    delete_day_keys(&redis, NaiveDate::from_ymd_opt(2027, 3, 21).unwrap()).await;
    delete_day_keys(&redis, NaiveDate::from_ymd_opt(2027, 3, 22).unwrap()).await;
    let day = NaiveDate::from_ymd_opt(2027, 3, 21).unwrap();
    usage_counters::increment(&pool, user_id, day, 5, 120)
        .await
        .expect("seed the DB row");
    set_redis(&redis, user_id, day, 8, 300).await;

    let disabled = ReconcileConfig {
        enabled: false,
        ..config()
    };
    let report = run_reconciliation(&pool, &redis, Arc::new(clock), &disabled)
        .await
        .expect("the run succeeds");
    assert!(!report.ran);
    assert_eq!(report.checked, 0);
    assert_eq!(report.corrected, 0);
    assert_eq!(report.audited, 0);
    assert_eq!(redis_totals(&redis, user_id, day).await, (8, 300));

    delete_key(&redis, user_id, day).await;
    teardown(pool, db).await;
}

/// One bad user-day must not abort the sweep: the day
/// whose Redis key is a plain string (not a hash) errors
/// and is counted, and the other day is still corrected.
#[tokio::test]
async fn one_bad_day_does_not_abort_the_sweep() {
    let db = "detag_p082_recon_bad_day";
    let pool = setup(db).await;
    let redis = redis_pool();
    let bad_user = seed_user(&pool).await;
    let good_user = seed_user(&pool).await;
    let clock = ManualClock::new(Utc.with_ymd_and_hms(2027, 3, 25, 2, 0, 0).unwrap());
    delete_day_keys(&redis, NaiveDate::from_ymd_opt(2027, 3, 24).unwrap()).await;
    delete_day_keys(&redis, NaiveDate::from_ymd_opt(2027, 3, 25).unwrap()).await;
    let day = NaiveDate::from_ymd_opt(2027, 3, 24).unwrap();
    usage_counters::increment(&pool, bad_user, day, 5, 120)
        .await
        .expect("seed the bad user's DB row");
    usage_counters::increment(&pool, good_user, day, 3, 60)
        .await
        .expect("seed the good user's DB row");
    // The bad day's key is a string, not a hash: HMGET
    // answers WRONGTYPE and reconcile_day fails.
    {
        let mut conn = redis.get().await.expect("checkout");
        let _: () = cmd("SET")
            .arg(usage_key(&bad_user, day))
            .arg("garbage")
            .query_async(&mut conn)
            .await
            .expect("SET the poisoned key");
    }
    // Δvideo 30 (< 60): corrected, not audited.
    set_redis(&redis, good_user, day, 3, 90).await;

    let report = run_reconciliation(&pool, &redis, Arc::new(clock), &config())
        .await
        .expect("the run succeeds");
    assert!(report.ran);
    assert_eq!(report.checked, 1, "only the good day completed");
    assert_eq!(report.corrected, 1);
    assert_eq!(report.audited, 0);
    assert_eq!(report.errors, 1, "the poisoned day is counted");
    assert_eq!(redis_totals(&redis, good_user, day).await, (3, 60));

    delete_key(&redis, bad_user, day).await;
    delete_key(&redis, good_user, day).await;
    teardown(pool, db).await;
}

/// Drift outside the window is untouched: the run covers
/// today + yesterday only, never an older day (its hash
/// is past the 48 h TTL anyway).
#[tokio::test]
async fn old_drift_is_outside_the_window() {
    let db = "detag_p082_recon_old_day";
    let pool = setup(db).await;
    let redis = redis_pool();
    let user_id = seed_user(&pool).await;
    // Run date 2027-03-28: the window is 03-27 + 03-28.
    // The drift sits on 03-26 — outside it.
    let clock = ManualClock::new(Utc.with_ymd_and_hms(2027, 3, 28, 2, 0, 0).unwrap());
    delete_day_keys(&redis, NaiveDate::from_ymd_opt(2027, 3, 26).unwrap()).await;
    delete_day_keys(&redis, NaiveDate::from_ymd_opt(2027, 3, 27).unwrap()).await;
    delete_day_keys(&redis, NaiveDate::from_ymd_opt(2027, 3, 28).unwrap()).await;
    let day = NaiveDate::from_ymd_opt(2027, 3, 26).unwrap();
    usage_counters::increment(&pool, user_id, day, 5, 120)
        .await
        .expect("seed the DB row");
    set_redis(&redis, user_id, day, 9, 400).await;

    let report = run_reconciliation(&pool, &redis, Arc::new(clock), &config())
        .await
        .expect("the run succeeds");
    assert!(report.ran);
    assert_eq!(report.checked, 0);
    assert_eq!(report.corrected, 0);
    assert_eq!(report.audited, 0);
    assert_eq!(redis_totals(&redis, user_id, day).await, (9, 400));

    delete_key(&redis, user_id, day).await;
    teardown(pool, db).await;
}
