//! The metering suite (P1-084, FR-P-07).
//!
//! One file that proves the four metering behaviours the
//! P1-050 / P1-079 / P1-082 tickets built separately, under
//! the conditions that break counters in production —
//! races, midnight, multi-job true-ups, and drift —
//! against the real services: a scratch Postgres DB per
//! test (created + dropped by the test, the `usage_live.rs`
//! pattern) and the dev Redis (`PF_REDIS_URL`, dev default
//! `redis://localhost:6379`). Keys and rows are per fresh
//! user UUID, so parallel tests never share a counter.
//!
//! The four legs of the ticket spec:
//!
//! - **Races (parallel creates under limit):**
//!   `parallel_records_under_the_limit_all_land` — eight
//!   concurrent single-image records on a Free user (limit
//!   10/day, read from the seeded catalog) land as exactly
//!   8 in Redis AND in the DB, so the durable snapshot a
//!   quota check reads would admit all eight and report 2
//!   remaining — no lost increment, no clobber.
//!   `parallel_records_racing_reconcile_never_lose_a_count`
//!   — twenty concurrent records race a reconcile loop;
//!   the DB holds every record exactly, and one reconcile
//!   after the records settle makes Redis agree with it
//!   (the hash may lag the DB while records are landing —
//!   a CAS can set it back to a DB value read before an
//!   in-flight record's upsert — and the settle +
//!   reconcile repairs it).
//! - **Midnight rollover (clock):**
//!   `rollover_splits_video_seconds_across_days` —
//!   records either side of UTC midnight land on their own
//!   days, in both stores, and the old day is untouched.
//!   `record_at_exact_midnight_lands_on_the_new_day` — a
//!   record at exactly 00:00:00 belongs to the new day.
//! - **True-up math:**
//!   `true_up_math_is_exact_across_many_jobs` — five jobs
//!   with mixed over/under estimates settle to exactly the
//!   sum of their actuals, in both stores.
//!   `true_up_refunds_floor_at_zero_across_jobs` —
//!   interleaved refunds floor at 0, and a retried
//!   completion true-up is a no-op.
//! - **Drift reconciliation:**
//!   `reconcile_repairs_both_legs_at_once` — a hash
//!   corrupted on both fields is repaired in one pass.
//!   `nightly_job_repairs_yesterday_drift_after_rollover`
//!   — the P1-082 job, run at 02:00 after a midnight
//!   crossing, corrects yesterday's drift AND audits it
//!   (the video leg is past the threshold).
//!
//! Isolation note — the nightly-job test enumerates
//! candidates with a Redis SCAN, so its window days must
//! not overlap any other suite's: every test here freezes
//! a distinct April-2027 date, spaced 3 days apart, and
//! the job test deletes its window days' keys first (a
//! VM-reset-killed run must not break the next run's
//! counts). No other suite freezes an April-2027 date.
//!
//! Honesty: the atomic check-and-record race — a quota
//! check immediately followed by the record, under a
//! limit, at the HTTP layer — is NOT proven here: the
//! check is read-only today (P1-051) and the atomic
//! check-and-record lands with real job creation in Phase
//! 2. This suite proves the meter-level properties that
//! check-and-record will rely on (exact totals under
//! concurrency, the durable snapshot reading exactly what
//! the meter recorded, reconcile never wiping a record).
//! The HTTP-level limit trip itself is already proven in
//! `apps/api/tests/entitlements.rs` (P1-057: Free 9/10
//! reaches the stub, 10/10 is 429 `PF-3001`).

use chrono::{NaiveDate, TimeZone, Utc};
use deadpool_redis::redis::cmd;
use detag_core::clock::ManualClock;
use detag_core::db::{connect, DbPool, PoolConfig};
use detag_core::migrate::migrate_up;
use detag_core::plans::{Entitlements, InMemoryPlanCache, PlanCatalog, SqlxPlanStore};
use detag_core::reconciliation::{run_reconciliation, ReconcileConfig, RECONCILE_AUDIT_ACTION};
use detag_core::redis::keys::usage_key;
use detag_core::redis::{create_pool, RedisPool, RedisPoolConfig};
use detag_core::repos::{audit_logs, usage_counters, users};
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
        "detag_p084_suite_under_limit" => "DROP DATABASE IF EXISTS detag_p084_suite_under_limit",
        "detag_p084_suite_race_reconcile" => {
            "DROP DATABASE IF EXISTS detag_p084_suite_race_reconcile"
        }
        "detag_p084_suite_rollover_split" => {
            "DROP DATABASE IF EXISTS detag_p084_suite_rollover_split"
        }
        "detag_p084_suite_exact_midnight" => {
            "DROP DATABASE IF EXISTS detag_p084_suite_exact_midnight"
        }
        "detag_p084_suite_trueup_many" => "DROP DATABASE IF EXISTS detag_p084_suite_trueup_many",
        "detag_p084_suite_trueup_floor" => "DROP DATABASE IF EXISTS detag_p084_suite_trueup_floor",
        "detag_p084_suite_reconcile_both" => {
            "DROP DATABASE IF EXISTS detag_p084_suite_reconcile_both"
        }
        "detag_p084_suite_nightly_rollover" => {
            "DROP DATABASE IF EXISTS detag_p084_suite_nightly_rollover"
        }
        _ => panic!("unknown P1-084 test db {db}"),
    }
}

fn create_db_sql(db: &str) -> &'static str {
    match db {
        "detag_p084_suite_under_limit" => "CREATE DATABASE detag_p084_suite_under_limit",
        "detag_p084_suite_race_reconcile" => "CREATE DATABASE detag_p084_suite_race_reconcile",
        "detag_p084_suite_rollover_split" => "CREATE DATABASE detag_p084_suite_rollover_split",
        "detag_p084_suite_exact_midnight" => "CREATE DATABASE detag_p084_suite_exact_midnight",
        "detag_p084_suite_trueup_many" => "CREATE DATABASE detag_p084_suite_trueup_many",
        "detag_p084_suite_trueup_floor" => "CREATE DATABASE detag_p084_suite_trueup_floor",
        "detag_p084_suite_reconcile_both" => "CREATE DATABASE detag_p084_suite_reconcile_both",
        "detag_p084_suite_nightly_rollover" => "CREATE DATABASE detag_p084_suite_nightly_rollover",
        _ => panic!("unknown P1-084 test db {db}"),
    }
}

async fn setup(db: &str) -> DbPool {
    let url = database_url();
    let admin = connect(&url, &PoolConfig::default())
        .await
        .unwrap_or_else(|err| {
            panic!(
                "live P1-084 test needs Postgres on {url}: {err} — start it with `just dev` (or the native postgres, see DOCKER-001)"
            )
        });
    sqlx::query(drop_db_sql(db))
        .execute(&admin)
        .await
        .expect("drop stale test db");
    sqlx::query(create_db_sql(db))
        .execute(&admin)
        .await
        .unwrap_or_else(|err| panic!("live P1-084 test needs CREATEDB for the pf role: {err}"));
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
    let email = format!("metering-{}@example.com", Uuid::now_v7());
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

async fn db_totals(pool: &DbPool, user_id: Uuid, day: NaiveDate) -> (i64, i64) {
    match usage_counters::for_user_day(pool, user_id, day)
        .await
        .expect("read the row")
    {
        Some(row) => (
            i64::from(row.images_used),
            i64::from(row.video_seconds_used),
        ),
        None => (0, 0),
    }
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
/// cleanup for the SCAN-based job test (the
/// `reconciliation_live.rs` pattern): a VM reset mid-test
/// leaves the previous run's hashes behind, and the job's
/// SCAN would find them, DB-miss them, and count them as
/// checked.
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

/// The seeded Free plan's daily image limit, read through
/// the app's own catalog (migration 003 seeds Free at 10
/// images/day — the same value `plans.rs` pins).
async fn free_daily_images(pool: &DbPool) -> i64 {
    let catalog = PlanCatalog::new(SqlxPlanStore::new(pool.clone()), InMemoryPlanCache::new());
    let plan = catalog
        .by_code("free")
        .await
        .expect("catalog reads")
        .expect("the Free plan is seeded");
    Entitlements::from_value(&plan.entitlements).daily_images
}

/// Races, under the limit: eight concurrent single-image
/// records on a Free user (limit 10/day) land as exactly
/// 8 in Redis AND in the DB — the durable snapshot a
/// quota check reads would admit all eight and report 2
/// remaining. A lost increment would read 7; a clobber
/// would read 1.
#[tokio::test]
async fn parallel_records_under_the_limit_all_land() {
    let db = "detag_p084_suite_under_limit";
    let pool = setup(db).await;
    let redis = redis_pool();
    let user_id = seed_user(&pool).await;
    let clock = ManualClock::new(Utc.with_ymd_and_hms(2027, 4, 6, 12, 0, 0).unwrap());
    let meter = Arc::new(UsageMeter::new(
        pool.clone(),
        redis.clone(),
        Arc::new(clock),
    ));
    let day = NaiveDate::from_ymd_opt(2027, 4, 6).unwrap();

    let limit = free_daily_images(&pool).await;
    assert_eq!(limit, 10, "Free seeds 10 images/day");
    let creates = 8;
    assert!(
        creates < limit,
        "the burst stays under the limit: {creates} < {limit}"
    );

    let mut tasks = Vec::new();
    for _ in 0..creates {
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

    // Exactly 8 in both stores — and the snapshot a quota
    // check reads agrees, so the check would see 2 left.
    assert_eq!(redis_totals(&redis, user_id, day).await, (8, 0));
    assert_eq!(db_totals(&pool, user_id, day).await, (8, 0));
    assert_eq!(limit - 8, 2, "two images of headroom remain");

    delete_key(&redis, user_id, day).await;
    teardown(pool, db).await;
}

/// Races, against reconcile: twenty concurrent records
/// race a reconcile loop on the same day. The durable
/// truth is never wiped — every record's DB upsert lands
/// exactly — and after the records settle, one reconcile
/// makes Redis agree with the DB. (A record whose Redis
/// increment lands between the reconcile's DB read and
/// its CAS can be transiently set back in the hash — the
/// CAS writes the DB value it read — so the hash may lag
/// the DB while records are still landing; the final
/// reconcile repairs it. The quota decisions read the
/// DB, never the hash, so the lag is never an
/// enforcement error.) A `Conflict` after the meter's 3
/// internal retries is retried by the caller here — it
/// is contention, not a lost count.
#[tokio::test]
async fn parallel_records_racing_reconcile_never_lose_a_count() {
    let db = "detag_p084_suite_race_reconcile";
    let pool = setup(db).await;
    let redis = redis_pool();
    let user_id = seed_user(&pool).await;
    let clock = ManualClock::new(Utc.with_ymd_and_hms(2027, 4, 9, 12, 0, 0).unwrap());
    let meter = Arc::new(UsageMeter::new(
        pool.clone(),
        redis.clone(),
        Arc::new(clock),
    ));
    let day = NaiveDate::from_ymd_opt(2027, 4, 9).unwrap();

    let mut tasks = Vec::new();
    for _ in 0..20 {
        let meter = Arc::clone(&meter);
        tasks.push(tokio::spawn(
            async move { meter.record(user_id, 1, 0).await },
        ));
    }
    // The reconcile loop runs while the records land.
    let racing = Arc::clone(&meter);
    let racer = tokio::spawn(async move {
        for _ in 0..30 {
            match racing.reconcile_day(user_id, day).await {
                Ok(_) => {}
                Err(UsageError::Conflict) => continue,
                Err(err) => panic!("reconcile under contention must not fail: {err:?}"),
            }
        }
    });
    for task in tasks {
        task.await.expect("task joins").expect("task succeeds");
    }
    racer.await.expect("racer joins");

    // The DB holds every record exactly — the durable
    // truth is never wiped by a reconcile. The hash may
    // lag it while records are still landing (a CAS can
    // set the hash back to a DB value read before an
    // in-flight record's upsert), so one reconcile after
    // the settle repairs the hash, and a second is a
    // no-op.
    assert_eq!(db_totals(&pool, user_id, day).await, (20, 0));
    let outcome = meter
        .reconcile_day(user_id, day)
        .await
        .expect("repair reconcile");
    assert_eq!(outcome.db_images, 20);
    assert_eq!(redis_totals(&redis, user_id, day).await, (20, 0));
    let outcome = meter
        .reconcile_day(user_id, day)
        .await
        .expect("final reconcile");
    assert!(!outcome.corrected, "no drift left to correct");
    assert_eq!(outcome.db_images, 20);

    delete_key(&redis, user_id, day).await;
    teardown(pool, db).await;
}

/// Midnight rollover, both legs: records either side of
/// UTC midnight land on their own days, in both stores,
/// and the old day is untouched by the new day's record.
#[tokio::test]
async fn rollover_splits_video_seconds_across_days() {
    let db = "detag_p084_suite_rollover_split";
    let pool = setup(db).await;
    let redis = redis_pool();
    let user_id = seed_user(&pool).await;
    let clock = ManualClock::new(Utc.with_ymd_and_hms(2027, 4, 12, 23, 59, 59).unwrap());
    let meter = UsageMeter::new(pool.clone(), redis.clone(), Arc::new(clock.clone()));
    let day_12 = NaiveDate::from_ymd_opt(2027, 4, 12).unwrap();
    let day_13 = NaiveDate::from_ymd_opt(2027, 4, 13).unwrap();

    // 23:59:59 UTC on the 12th: 2 images + 120 video
    // seconds land on the 12th, in both stores.
    let row = meter
        .record(user_id, 2, 120)
        .await
        .expect("record on the 12th");
    assert_eq!(row.day, day_12);
    assert_eq!(redis_totals(&redis, user_id, day_12).await, (2, 120));
    assert_eq!(db_totals(&pool, user_id, day_12).await, (2, 120));

    // Two seconds later — past UTC midnight — the same
    // user records again, into the 13th.
    clock.advance(chrono::Duration::seconds(2));
    let row = meter
        .record(user_id, 1, 45)
        .await
        .expect("record on the 13th");
    assert_eq!(row.day, day_13);
    assert_eq!(redis_totals(&redis, user_id, day_13).await, (1, 45));
    assert_eq!(db_totals(&pool, user_id, day_13).await, (1, 45));

    // And the 12th's counters are untouched by the 13th's
    // record — the reset is a new key/row, not a decrement.
    assert_eq!(redis_totals(&redis, user_id, day_12).await, (2, 120));
    assert_eq!(db_totals(&pool, user_id, day_12).await, (2, 120));

    delete_key(&redis, user_id, day_12).await;
    delete_key(&redis, user_id, day_13).await;
    teardown(pool, db).await;
}

/// The boundary itself is inclusive of the new day: a
/// record at exactly 00:00:00 UTC belongs to the new day,
/// not the old one.
#[tokio::test]
async fn record_at_exact_midnight_lands_on_the_new_day() {
    let db = "detag_p084_suite_exact_midnight";
    let pool = setup(db).await;
    let redis = redis_pool();
    let user_id = seed_user(&pool).await;
    let clock = ManualClock::new(Utc.with_ymd_and_hms(2027, 4, 15, 23, 59, 59).unwrap());
    let meter = UsageMeter::new(pool.clone(), redis.clone(), Arc::new(clock.clone()));
    let day_15 = NaiveDate::from_ymd_opt(2027, 4, 15).unwrap();
    let day_16 = NaiveDate::from_ymd_opt(2027, 4, 16).unwrap();

    meter
        .record(user_id, 1, 0)
        .await
        .expect("record at 23:59:59");
    assert_eq!(db_totals(&pool, user_id, day_15).await, (1, 0));

    // Exactly one second later — 00:00:00 on the 16th.
    clock.advance(chrono::Duration::seconds(1));
    assert_eq!(meter.today(), day_16, "the clock says the 16th");
    let row = meter
        .record(user_id, 1, 0)
        .await
        .expect("record at 00:00:00");
    assert_eq!(row.day, day_16);
    assert_eq!(db_totals(&pool, user_id, day_16).await, (1, 0));
    assert_eq!(redis_totals(&redis, user_id, day_16).await, (1, 0));
    // The 15th still holds exactly its one record.
    assert_eq!(db_totals(&pool, user_id, day_15).await, (1, 0));
    assert_eq!(redis_totals(&redis, user_id, day_15).await, (1, 0));

    delete_key(&redis, user_id, day_15).await;
    delete_key(&redis, user_id, day_16).await;
    teardown(pool, db).await;
}

/// True-up math, across many jobs: five jobs with mixed
/// over/under estimates settle to exactly the sum of
/// their actuals, in both stores — each true-up's applied
/// delta is asserted on the way.
#[tokio::test]
async fn true_up_math_is_exact_across_many_jobs() {
    let db = "detag_p084_suite_trueup_many";
    let pool = setup(db).await;
    let redis = redis_pool();
    let user_id = seed_user(&pool).await;
    let clock = ManualClock::new(Utc.with_ymd_and_hms(2027, 4, 19, 12, 0, 0).unwrap());
    let meter = UsageMeter::new(pool.clone(), redis.clone(), Arc::new(clock));
    let day = NaiveDate::from_ymd_opt(2027, 4, 19).unwrap();

    // (estimate, actual) per job; the day must end at the
    // sum of the actuals: 150 + 60 + 30 + 45 + 0 = 285.
    let jobs = [(90, 150), (60, 60), (120, 30), (0, 45), (30, 0)];
    let expected_deltas = [60, 0, -90, 45, -30];
    for (i, &(estimated, actual)) in jobs.iter().enumerate() {
        meter
            .record(user_id, 0, estimated)
            .await
            .expect("reserve the estimate");
        let outcome = meter
            .true_up(user_id, day, estimated, actual)
            .await
            .expect("true up");
        assert_eq!(
            outcome.delta_applied, expected_deltas[i],
            "job {i}: ({estimated} -> {actual}) applies {}",
            expected_deltas[i]
        );
        assert_eq!(outcome.day, day);
    }

    assert_eq!(redis_totals(&redis, user_id, day).await, (0, 285));
    assert_eq!(db_totals(&pool, user_id, day).await, (0, 285));

    delete_key(&redis, user_id, day).await;
    teardown(pool, db).await;
}

/// True-up math, the floor across jobs: interleaved
/// refunds floor the day total at 0, and a retried
/// completion true-up for an already-settled job is a
/// no-op — the second refund cannot drive the counter
/// below zero.
#[tokio::test]
async fn true_up_refunds_floor_at_zero_across_jobs() {
    let db = "detag_p084_suite_trueup_floor";
    let pool = setup(db).await;
    let redis = redis_pool();
    let user_id = seed_user(&pool).await;
    let clock = ManualClock::new(Utc.with_ymd_and_hms(2027, 4, 22, 12, 0, 0).unwrap());
    let meter = UsageMeter::new(pool.clone(), redis.clone(), Arc::new(clock));
    let day = NaiveDate::from_ymd_opt(2027, 4, 22).unwrap();

    // Two jobs reserve 90 + 10 = 100 s.
    meter.record(user_id, 0, 90).await.expect("reserve job A");
    meter.record(user_id, 0, 10).await.expect("reserve job B");
    assert_eq!(db_totals(&pool, user_id, day).await, (0, 100));

    // Job A ran 0 s: refund the whole 90, total 10.
    let outcome = meter.true_up(user_id, day, 90, 0).await.expect("true up A");
    assert_eq!(outcome.delta_applied, -90);
    assert_eq!(outcome.video_seconds_used, 10);
    assert_eq!(redis_totals(&redis, user_id, day).await, (0, 10));

    // Job B ran 0 s: refund the last 10, total 0.
    let outcome = meter.true_up(user_id, day, 10, 0).await.expect("true up B");
    assert_eq!(outcome.delta_applied, -10);
    assert_eq!(outcome.video_seconds_used, 0);
    assert_eq!(redis_totals(&redis, user_id, day).await, (0, 0));

    // Job A's completion retried (a duplicate delivery):
    // estimated 90, actual 0 against a 0 total — the floor
    // eats the whole refund, nothing moves.
    let outcome = meter
        .true_up(user_id, day, 90, 0)
        .await
        .expect("true up A again");
    assert_eq!(outcome.delta_applied, 0);
    assert_eq!(outcome.video_seconds_used, 0);
    assert_eq!(outcome.redis_video_seconds, 0);
    assert_eq!(db_totals(&pool, user_id, day).await, (0, 0));
    assert_eq!(redis_totals(&redis, user_id, day).await, (0, 0));

    delete_key(&redis, user_id, day).await;
    teardown(pool, db).await;
}

/// Drift reconciliation, both legs at once: a hash
/// corrupted on images AND video-seconds is repaired in
/// one pass, and the outcome reports the DB truth and
/// the before values on both legs.
#[tokio::test]
async fn reconcile_repairs_both_legs_at_once() {
    let db = "detag_p084_suite_reconcile_both";
    let pool = setup(db).await;
    let redis = redis_pool();
    let user_id = seed_user(&pool).await;
    let clock = ManualClock::new(Utc.with_ymd_and_hms(2027, 4, 25, 12, 0, 0).unwrap());
    let meter = UsageMeter::new(pool.clone(), redis.clone(), Arc::new(clock));
    let day = NaiveDate::from_ymd_opt(2027, 4, 25).unwrap();

    meter.record(user_id, 3, 120).await.expect("record");
    assert_eq!(redis_totals(&redis, user_id, day).await, (3, 120));

    // Corrupt both fields behind the meter's back.
    {
        let mut conn = redis.get().await.expect("checkout");
        let _: () = cmd("HSET")
            .arg(usage_key(&user_id, day))
            .arg("images")
            .arg(99)
            .arg("video_seconds")
            .arg(5)
            .query_async(&mut conn)
            .await
            .expect("corrupt the hash");
    }
    assert_eq!(redis_totals(&redis, user_id, day).await, (99, 5));

    let outcome = meter.reconcile_day(user_id, day).await.expect("reconcile");
    assert!(outcome.corrected, "drift must be corrected");
    assert_eq!(outcome.db_images, 3);
    assert_eq!(outcome.db_video_seconds, 120);
    assert_eq!(outcome.redis_images_before, 99);
    assert_eq!(outcome.redis_video_seconds_before, 5);
    assert_eq!(redis_totals(&redis, user_id, day).await, (3, 120));
    assert_eq!(db_totals(&pool, user_id, day).await, (3, 120));

    // A second reconcile is a no-op: the stores agree.
    let outcome = meter
        .reconcile_day(user_id, day)
        .await
        .expect("reconcile again");
    assert!(!outcome.corrected, "no drift left to correct");

    delete_key(&redis, user_id, day).await;
    teardown(pool, db).await;
}

/// Drift reconciliation, end to end through the nightly
/// job across a midnight crossing: usage recorded on the
/// 28th drifts in Redis, the clock crosses midnight to
/// 02:00 on the 29th, and the job still covers yesterday
/// — it corrects the hash AND audits it, because the
/// video leg (Δ60) reaches the audit threshold.
#[tokio::test]
async fn nightly_job_repairs_yesterday_drift_after_rollover() {
    let db = "detag_p084_suite_nightly_rollover";
    let pool = setup(db).await;
    let redis = redis_pool();
    let user_id = seed_user(&pool).await;
    // Frozen at 23:59 on 2027-04-28: the record lands on
    // the 28th.
    let clock = ManualClock::new(Utc.with_ymd_and_hms(2027, 4, 28, 23, 59, 0).unwrap());
    let meter = UsageMeter::new(pool.clone(), redis.clone(), Arc::new(clock.clone()));
    let day_28 = NaiveDate::from_ymd_opt(2027, 4, 28).unwrap();
    let day_29 = NaiveDate::from_ymd_opt(2027, 4, 29).unwrap();
    delete_day_keys(&redis, day_28).await;
    delete_day_keys(&redis, day_29).await;

    meter.record(user_id, 4, 200).await.expect("record");
    assert_eq!(redis_totals(&redis, user_id, day_28).await, (4, 200));

    // Drift behind the meter's back: Δimages 3 (< 10),
    // Δvideo 60 (>= 60) — the correction is audited.
    {
        let mut conn = redis.get().await.expect("checkout");
        let _: () = cmd("HSET")
            .arg(usage_key(&user_id, day_28))
            .arg("images")
            .arg(7)
            .arg("video_seconds")
            .arg(260)
            .query_async(&mut conn)
            .await
            .expect("corrupt the hash");
    }
    assert_eq!(redis_totals(&redis, user_id, day_28).await, (7, 260));

    // Past midnight to the nightly hour on the 29th: the
    // window is 04-28 + 04-29, so yesterday's drift is
    // still in scope.
    clock.set(Utc.with_ymd_and_hms(2027, 4, 29, 2, 0, 0).unwrap());
    let config = ReconcileConfig {
        enabled: true,
        audit_threshold_images: 10,
        audit_threshold_video_seconds: 60,
    };
    let report = run_reconciliation(&pool, &redis, Arc::new(clock), &config)
        .await
        .expect("the run succeeds");
    assert!(report.ran);
    assert_eq!(report.window_start, day_28);
    assert_eq!(report.window_end, day_29);
    assert_eq!(report.checked, 1, "only the drifted day is a candidate");
    assert_eq!(report.corrected, 1);
    assert_eq!(report.audited, 1);
    assert_eq!(report.errors, 0);
    assert_eq!(report.anomalies.len(), 1);
    assert_eq!(redis_totals(&redis, user_id, day_28).await, (4, 200));
    // The 29th is untouched — no row, no hash.
    assert_eq!(db_totals(&pool, user_id, day_29).await, (0, 0));
    assert_eq!(redis_totals(&redis, user_id, day_29).await, (0, 0));

    let rows = audit_logs::list_for_action(&pool, RECONCILE_AUDIT_ACTION)
        .await
        .expect("read the audit rows");
    assert_eq!(rows.len(), 1);
    let before = rows[0].before.as_ref().expect("before payload");
    assert_eq!(before["images"], 7);
    assert_eq!(before["video_seconds"], 260);
    let after = rows[0].after.as_ref().expect("after payload");
    assert_eq!(after["images"], 4);
    assert_eq!(after["video_seconds"], 200);

    delete_key(&redis, user_id, day_28).await;
    delete_key(&redis, user_id, day_29).await;
    teardown(pool, db).await;
}
