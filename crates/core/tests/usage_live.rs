//! Live usage-metering tests (P1-050, FR-P-02 / FR-P-07).
//!
//! The meter writes Redis (atomic Lua) + Postgres
//! (write-through) on the injected clock's UTC day, so
//! these tests run against the real services: a scratch
//! Postgres DB per test (created + dropped by the test,
//! the `repos_live.rs` pattern — never the dev database)
//! and the dev Redis (`PF_REDIS_URL`, dev default
//! `redis://localhost:6379`). Keys and rows are per fresh
//! user UUID, so parallel tests never share a counter.
//!
//! - `utc_boundary_resets_at_midnight` — the ticket's AC:
//!   a frozen clock at 23:59:59 UTC records into the 15th;
//!   two seconds later (past midnight) the same user
//!   records into the 16th, and the 15th's counters are
//!   untouched — in Redis AND in the DB.
//! - `reconcile_repairs_redis_drift` — a Redis hash
//!   corrupted behind the meter's back is overwritten with
//!   the DB values (DB wins); a second reconcile is a
//!   no-op.
//! - `concurrent_records_do_not_lose_counts` — ten
//!   concurrent single-image records land as exactly 10 in
//!   Redis and in the DB (the Lua increment is atomic).
//!
//! P1-079 (FR-P-07) adds the completion true-up
//! ([`detag_core::usage::UsageMeter::true_up`]): the
//! estimate reserved at create is replaced by the real
//! duration on the job's CREATE day.
//!
//! - `true_up_charges_the_extra_seconds` — estimate 90,
//!   actual 150: +60 in Redis AND in the DB.
//! - `true_up_refunds_the_over_estimate` — estimate 90,
//!   actual 30: −60 in both stores.
//! - `true_up_never_drives_the_counter_negative` — a
//!   refund larger than the day's total floors at 0 in
//!   both stores; a second true-up is a no-op.
//! - `true_up_lands_on_the_create_day` — the clock crosses
//!   UTC midnight between create and completion, but the
//!   true-up corrects the create day and leaves the new
//!   day untouched.

use chrono::{NaiveDate, TimeZone, Utc};
use deadpool_redis::redis::cmd;
use detag_core::clock::ManualClock;
use detag_core::db::{connect, DbPool, PoolConfig};
use detag_core::migrate::migrate_up;
use detag_core::redis::keys::usage_key;
use detag_core::redis::{create_pool, RedisPool, RedisPoolConfig};
use detag_core::repos::{usage_counters, users};
use detag_core::usage::{UsageError, UsageMeter};
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
        "detag_p050_usage_boundary" => "DROP DATABASE IF EXISTS detag_p050_usage_boundary",
        "detag_p050_usage_reconcile" => "DROP DATABASE IF EXISTS detag_p050_usage_reconcile",
        "detag_p050_usage_concurrent" => "DROP DATABASE IF EXISTS detag_p050_usage_concurrent",
        "detag_p050_usage_dead_redis" => "DROP DATABASE IF EXISTS detag_p050_usage_dead_redis",
        "detag_p079_usage_true_up_over" => "DROP DATABASE IF EXISTS detag_p079_usage_true_up_over",
        "detag_p079_usage_true_up_under" => {
            "DROP DATABASE IF EXISTS detag_p079_usage_true_up_under"
        }
        "detag_p079_usage_true_up_floor" => {
            "DROP DATABASE IF EXISTS detag_p079_usage_true_up_floor"
        }
        "detag_p079_usage_true_up_day" => "DROP DATABASE IF EXISTS detag_p079_usage_true_up_day",
        _ => panic!("unknown P1-050 test db {db}"),
    }
}

fn create_db_sql(db: &str) -> &'static str {
    match db {
        "detag_p050_usage_boundary" => "CREATE DATABASE detag_p050_usage_boundary",
        "detag_p050_usage_reconcile" => "CREATE DATABASE detag_p050_usage_reconcile",
        "detag_p050_usage_concurrent" => "CREATE DATABASE detag_p050_usage_concurrent",
        "detag_p050_usage_dead_redis" => "CREATE DATABASE detag_p050_usage_dead_redis",
        "detag_p079_usage_true_up_over" => "CREATE DATABASE detag_p079_usage_true_up_over",
        "detag_p079_usage_true_up_under" => "CREATE DATABASE detag_p079_usage_true_up_under",
        "detag_p079_usage_true_up_floor" => "CREATE DATABASE detag_p079_usage_true_up_floor",
        "detag_p079_usage_true_up_day" => "CREATE DATABASE detag_p079_usage_true_up_day",
        _ => panic!("unknown P1-050 test db {db}"),
    }
}

async fn setup(db: &str) -> DbPool {
    let url = database_url();
    let admin = connect(&url, &PoolConfig::default())
        .await
        .unwrap_or_else(|err| {
            panic!(
                "live P1-050 test needs Postgres on {url}: {err} — start it with `just dev` (or the native postgres, see DOCKER-001)"
            )
        });
    sqlx::query(drop_db_sql(db))
        .execute(&admin)
        .await
        .expect("drop stale test db");
    sqlx::query(create_db_sql(db))
        .execute(&admin)
        .await
        .unwrap_or_else(|err| panic!("live P1-050 test needs CREATEDB for the pf role: {err}"));
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
    let email = format!("usage-{}@example.com", Uuid::now_v7());
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

async fn delete_key(pool: &RedisPool, user_id: Uuid, day: NaiveDate) {
    let mut conn = pool.get().await.expect("checkout");
    let _: () = cmd("DEL")
        .arg(usage_key(&user_id, day))
        .query_async(&mut conn)
        .await
        .expect("DEL the day hash");
}

/// The ticket's AC: the UTC boundary, on a clock the test
/// moves by hand.
#[tokio::test]
async fn utc_boundary_resets_at_midnight() {
    let db = "detag_p050_usage_boundary";
    let pool = setup(db).await;
    let redis = redis_pool();
    let user_id = seed_user(&pool).await;
    let clock = ManualClock::new(Utc.with_ymd_and_hms(2026, 11, 15, 23, 59, 59).unwrap());
    let meter = UsageMeter::new(pool.clone(), redis.clone(), Arc::new(clock.clone()));
    let day_15 = NaiveDate::from_ymd_opt(2026, 11, 15).unwrap();
    let day_16 = NaiveDate::from_ymd_opt(2026, 11, 16).unwrap();

    // 23:59:59 UTC on the 15th: one image + 90 video seconds
    // land on the 15th, in both stores.
    let row = meter
        .record(user_id, 1, 90)
        .await
        .expect("record on the 15th");
    assert_eq!(row.day, day_15);
    assert_eq!(row.images_used, 1);
    assert_eq!(row.video_seconds_used, 90);
    assert_eq!(redis_totals(&redis, user_id, day_15).await, (1, 90));

    // Two seconds later — past UTC midnight — the same user
    // records again, into the 16th.
    clock.advance(chrono::Duration::seconds(2));
    let row = meter
        .record(user_id, 1, 30)
        .await
        .expect("record on the 16th");
    assert_eq!(row.day, day_16);
    assert_eq!(row.images_used, 1);
    assert_eq!(row.video_seconds_used, 30);
    assert_eq!(redis_totals(&redis, user_id, day_16).await, (1, 30));

    // And the 15th's counters are untouched by the 16th's
    // record — the reset is a new key/row, not a decrement.
    let old_row = usage_counters::for_user_day(&pool, user_id, day_15)
        .await
        .expect("read the 15th's row")
        .expect("the 15th's row exists");
    assert_eq!(old_row.images_used, 1);
    assert_eq!(old_row.video_seconds_used, 90);
    assert_eq!(redis_totals(&redis, user_id, day_15).await, (1, 90));

    delete_key(&redis, user_id, day_15).await;
    delete_key(&redis, user_id, day_16).await;
    teardown(pool, db).await;
}

#[tokio::test]
async fn reconcile_repairs_redis_drift() {
    let db = "detag_p050_usage_reconcile";
    let pool = setup(db).await;
    let redis = redis_pool();
    let user_id = seed_user(&pool).await;
    let clock = ManualClock::new(Utc.with_ymd_and_hms(2026, 11, 15, 12, 0, 0).unwrap());
    let meter = UsageMeter::new(pool.clone(), redis.clone(), Arc::new(clock));
    let day = NaiveDate::from_ymd_opt(2026, 11, 15).unwrap();

    meter.record(user_id, 3, 120).await.expect("record");
    assert_eq!(redis_totals(&redis, user_id, day).await, (3, 120));

    // Corrupt Redis behind the meter's back (a lost DB
    // write, a crashed process, a manual edit).
    let mut conn = redis.get().await.expect("checkout");
    let _: () = cmd("HSET")
        .arg(usage_key(&user_id, day))
        .arg("images")
        .arg(99)
        .query_async(&mut conn)
        .await
        .expect("corrupt the hash");
    drop(conn);
    assert_eq!(redis_totals(&redis, user_id, day).await, (99, 120));

    // Reconcile: the DB wins, Redis is corrected.
    let outcome = meter.reconcile_day(user_id, day).await.expect("reconcile");
    assert!(outcome.corrected, "drift must be corrected");
    assert_eq!(outcome.db_images, 3);
    assert_eq!(outcome.db_video_seconds, 120);
    assert_eq!(outcome.redis_images_before, 99);
    assert_eq!(redis_totals(&redis, user_id, day).await, (3, 120));

    // A second reconcile is a no-op: the stores agree.
    let outcome = meter
        .reconcile_day(user_id, day)
        .await
        .expect("reconcile again");
    assert!(!outcome.corrected, "no drift left to correct");

    delete_key(&redis, user_id, day).await;
    teardown(pool, db).await;
}

#[tokio::test]
async fn concurrent_records_do_not_lose_counts() {
    let db = "detag_p050_usage_concurrent";
    let pool = setup(db).await;
    let redis = redis_pool();
    let user_id = seed_user(&pool).await;
    let clock = ManualClock::new(Utc.with_ymd_and_hms(2026, 11, 15, 12, 0, 0).unwrap());
    let meter = Arc::new(UsageMeter::new(
        pool.clone(),
        redis.clone(),
        Arc::new(clock),
    ));
    let day = NaiveDate::from_ymd_opt(2026, 11, 15).unwrap();

    // Ten concurrent single-image records: the atomic Lua
    // increment + the upsert must land as exactly 10 in
    // both stores, not 9 (a lost update) or 1 (a clobber).
    let mut tasks = Vec::new();
    for _ in 0..10 {
        let meter = Arc::clone(&meter);
        tasks.push(tokio::spawn(
            async move { meter.record(user_id, 1, 0).await },
        ));
    }
    for task in tasks {
        task.await
            .expect("record task joins")
            .expect("record succeeds");
    }
    assert_eq!(redis_totals(&redis, user_id, day).await, (10, 0));
    let row = usage_counters::for_user_day(&pool, user_id, day)
        .await
        .expect("read the row")
        .expect("the row exists");
    assert_eq!(row.images_used, 10);
    assert_eq!(row.video_seconds_used, 0);

    delete_key(&redis, user_id, day).await;
    teardown(pool, db).await;
}

/// A meter whose Redis is unreachable fails closed on
/// record: the error surfaces instead of a silent
/// uncounted job (the caller — job-create — decides how
/// to surface it; the counter is never guessed).
#[tokio::test]
async fn record_fails_closed_when_redis_is_down() {
    let db = "detag_p050_usage_dead_redis";
    let pool = setup(db).await;
    let dead_redis = create_pool("redis://localhost:6399", &RedisPoolConfig::default())
        .expect("pool builds from any URL");
    let user_id = seed_user(&pool).await;
    let clock = ManualClock::new(Utc.with_ymd_and_hms(2026, 11, 15, 12, 0, 0).unwrap());
    let meter = UsageMeter::new(pool.clone(), dead_redis, Arc::new(clock));
    let err = meter
        .record(user_id, 1, 0)
        .await
        .expect_err("record against a dead Redis must error");
    assert!(
        matches!(err, UsageError::Pool(_) | UsageError::Redis(_)),
        "a dead Redis is a Pool/Redis error, not a silent zero: {err:?}"
    );
    // And nothing was written to the DB either — the record
    // is all-or-nothing from the caller's view (the Redis
    // write happens first, so a Redis failure never reaches
    // the DB).
    let row = usage_counters::for_user_day(
        &pool,
        user_id,
        NaiveDate::from_ymd_opt(2026, 11, 15).unwrap(),
    )
    .await
    .expect("read the row");
    assert!(row.is_none(), "no DB row when Redis failed");
    teardown(pool, db).await;
}

/// P1-079, the ticket's AC (over-estimate case): a video
/// estimated at 90 s runs 150 s — the true-up charges
/// the extra 60 s, in Redis AND in the DB.
#[tokio::test]
async fn true_up_charges_the_extra_seconds() {
    let db = "detag_p079_usage_true_up_over";
    let pool = setup(db).await;
    let redis = redis_pool();
    let user_id = seed_user(&pool).await;
    let clock = ManualClock::new(Utc.with_ymd_and_hms(2026, 11, 15, 12, 0, 0).unwrap());
    let meter = UsageMeter::new(pool.clone(), redis.clone(), Arc::new(clock));
    let day = NaiveDate::from_ymd_opt(2026, 11, 15).unwrap();

    meter.record(user_id, 0, 90).await.expect("reserve 90 s");
    let outcome = meter.true_up(user_id, day, 90, 150).await.expect("true up");
    assert_eq!(outcome.day, day);
    assert_eq!(outcome.estimated_seconds, 90);
    assert_eq!(outcome.actual_seconds, 150);
    assert_eq!(outcome.delta_applied, 60);
    assert_eq!(outcome.video_seconds_used, 150);
    assert_eq!(outcome.redis_video_seconds, 150);
    assert_eq!(redis_totals(&redis, user_id, day).await, (0, 150));
    let row = usage_counters::for_user_day(&pool, user_id, day)
        .await
        .expect("read the row")
        .expect("the row exists");
    assert_eq!(row.video_seconds_used, 150);

    delete_key(&redis, user_id, day).await;
    teardown(pool, db).await;
}

/// P1-079, the ticket's AC (under-estimate case): a video
/// estimated at 90 s runs 30 s — the true-up refunds 60 s,
/// in Redis AND in the DB.
#[tokio::test]
async fn true_up_refunds_the_over_estimate() {
    let db = "detag_p079_usage_true_up_under";
    let pool = setup(db).await;
    let redis = redis_pool();
    let user_id = seed_user(&pool).await;
    let clock = ManualClock::new(Utc.with_ymd_and_hms(2026, 11, 15, 12, 0, 0).unwrap());
    let meter = UsageMeter::new(pool.clone(), redis.clone(), Arc::new(clock));
    let day = NaiveDate::from_ymd_opt(2026, 11, 15).unwrap();

    meter.record(user_id, 0, 90).await.expect("reserve 90 s");
    let outcome = meter.true_up(user_id, day, 90, 30).await.expect("true up");
    assert_eq!(outcome.delta_applied, -60);
    assert_eq!(outcome.video_seconds_used, 30);
    assert_eq!(outcome.redis_video_seconds, 30);
    assert_eq!(redis_totals(&redis, user_id, day).await, (0, 30));
    let row = usage_counters::for_user_day(&pool, user_id, day)
        .await
        .expect("read the row")
        .expect("the row exists");
    assert_eq!(row.video_seconds_used, 30);

    delete_key(&redis, user_id, day).await;
    teardown(pool, db).await;
}

/// P1-079: a refund larger than the day's total floors at
/// 0 — neither store ever goes negative, and a second
/// true-up for the same (already settled) job is a no-op.
#[tokio::test]
async fn true_up_never_drives_the_counter_negative() {
    let db = "detag_p079_usage_true_up_floor";
    let pool = setup(db).await;
    let redis = redis_pool();
    let user_id = seed_user(&pool).await;
    let clock = ManualClock::new(Utc.with_ymd_and_hms(2026, 11, 15, 12, 0, 0).unwrap());
    let meter = UsageMeter::new(pool.clone(), redis.clone(), Arc::new(clock));
    let day = NaiveDate::from_ymd_opt(2026, 11, 15).unwrap();

    // Only 10 s were ever reserved; the video ran 0 s,
    // so the refund is the whole 10 — no further.
    meter.record(user_id, 0, 10).await.expect("reserve 10 s");
    let outcome = meter.true_up(user_id, day, 10, 0).await.expect("true up");
    assert_eq!(outcome.delta_applied, -10);
    assert_eq!(outcome.video_seconds_used, 0);
    assert_eq!(outcome.redis_video_seconds, 0);
    assert_eq!(redis_totals(&redis, user_id, day).await, (0, 0));

    // The same job true-up'd again (a retried completion):
    // estimated 10, actual 0 against a 0 total — the floor
    // eats the whole refund, nothing moves.
    let outcome = meter
        .true_up(user_id, day, 10, 0)
        .await
        .expect("true up again");
    assert_eq!(outcome.delta_applied, 0);
    assert_eq!(outcome.video_seconds_used, 0);
    assert_eq!(outcome.redis_video_seconds, 0);
    assert_eq!(redis_totals(&redis, user_id, day).await, (0, 0));
    let row = usage_counters::for_user_day(&pool, user_id, day)
        .await
        .expect("read the row")
        .expect("the row exists");
    assert_eq!(row.video_seconds_used, 0);

    delete_key(&redis, user_id, day).await;
    teardown(pool, db).await;
}

/// P1-079 (ASSUMPTION-040): the true-up lands on the
/// job's CREATE day even when completion crosses UTC
/// midnight — the new day's counters are untouched.
#[tokio::test]
async fn true_up_lands_on_the_create_day() {
    let db = "detag_p079_usage_true_up_day";
    let pool = setup(db).await;
    let redis = redis_pool();
    let user_id = seed_user(&pool).await;
    let clock = ManualClock::new(Utc.with_ymd_and_hms(2026, 11, 15, 23, 59, 59).unwrap());
    let meter = UsageMeter::new(pool.clone(), redis.clone(), Arc::new(clock.clone()));
    let day_15 = NaiveDate::from_ymd_opt(2026, 11, 15).unwrap();
    let day_16 = NaiveDate::from_ymd_opt(2026, 11, 16).unwrap();

    // 23:59:59 UTC on the 15th: 60 s reserved on the 15th.
    meter.record(user_id, 0, 60).await.expect("reserve 60 s");

    // Two seconds later — past UTC midnight — the job
    // completes at 120 s. The true-up names the 15th.
    clock.advance(chrono::Duration::seconds(2));
    let outcome = meter
        .true_up(user_id, day_15, 60, 120)
        .await
        .expect("true up");
    assert_eq!(outcome.day, day_15);
    assert_eq!(outcome.delta_applied, 60);
    assert_eq!(outcome.video_seconds_used, 120);
    assert_eq!(redis_totals(&redis, user_id, day_15).await, (0, 120));

    // The 16th is untouched — no row, no hash.
    let row_16 = usage_counters::for_user_day(&pool, user_id, day_16)
        .await
        .expect("read the 16th's row");
    assert!(row_16.is_none(), "no 16th row from a 15th true-up");
    assert_eq!(redis_totals(&redis, user_id, day_16).await, (0, 0));

    delete_key(&redis, user_id, day_15).await;
    delete_key(&redis, user_id, day_16).await;
    teardown(pool, db).await;
}
