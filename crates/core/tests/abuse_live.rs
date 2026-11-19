//! Live abuse-spike-detection tests (P1-083, FR-P-07).
//!
//! The velocity rule counts a user's `jobs` rows created inside
//! the trailing window; at the threshold it files one
//! `abuse_reports` row (status `open`) + one
//! `abuse.spike_detected` audit row, and — only when
//! `PF_ABUSE_AUTO_SUSPEND` is on — suspends the account
//! through the full P1-008 sequence. These tests run against
//! the real services: a scratch Postgres DB per test (created +
//! dropped by the test, the `usage_live.rs` pattern). No Redis
//! leg: the `jobs` table is the source of truth for creations.
//!
//! The clock is frozen at real now (`ManualClock::new(Utc::now())`)
//! so the DB-stamped `created_at` of freshly seeded jobs falls
//! inside the 60 s window; the old-jobs test moves the rows back
//! with an UPDATE instead. Each test uses a small threshold (3)
//! so seeding stays cheap — the production default (30) is
//! covered by the unit tests in `abuse.rs`.
//!
//! - `spike_detected` — the ticket's AC: 3 jobs in 60 s files
//!   exactly one report + one spike audit row.
//! - `flag_off_leaves_account_active` — the ticket's second AC:
//!   with auto-suspend off (the default) the spiking user stays
//!   `active` and no `user.suspended` audit row exists.
//! - `under_threshold_is_quiet` — 2 of 3 jobs files nothing.
//! - `second_check_in_window_does_not_duplicate` — a repeat
//!   check in the same window returns the existing report and
//!   writes nothing new.
//! - `old_jobs_outside_window_do_not_count` — jobs created 10
//!   minutes ago do not trip a 60 s window.
//! - `auto_suspend_suspends_when_on` — with the flag on, the
//!   spike suspends the account (status + sessions revoked +
//!   queued jobs held, audited as `user.suspended`).
//! - `disabled_detector_files_nothing` — the kill-switch files
//!   nothing even over threshold.

use chrono::Utc;
use detag_core::abuse::{check_velocity, AbuseConfig, SPIKE_AUDIT_ACTION};
use detag_core::clock::ManualClock;
use detag_core::db::{connect, DbPool, PoolConfig};
use detag_core::migrate::migrate_up;
use detag_core::repos::{abuse_reports, audit_logs, jobs, users};
use sqlx::Row;
use std::path::PathBuf;
use std::sync::Arc;
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
        "detag_p083_abuse_ac" => "DROP DATABASE IF EXISTS detag_p083_abuse_ac",
        "detag_p083_abuse_flag_off" => "DROP DATABASE IF EXISTS detag_p083_abuse_flag_off",
        "detag_p083_abuse_quiet" => "DROP DATABASE IF EXISTS detag_p083_abuse_quiet",
        "detag_p083_abuse_dedup" => "DROP DATABASE IF EXISTS detag_p083_abuse_dedup",
        "detag_p083_abuse_old" => "DROP DATABASE IF EXISTS detag_p083_abuse_old",
        "detag_p083_abuse_suspend" => "DROP DATABASE IF EXISTS detag_p083_abuse_suspend",
        "detag_p083_abuse_disabled" => "DROP DATABASE IF EXISTS detag_p083_abuse_disabled",
        _ => panic!("unknown P1-083 test db {db}"),
    }
}

fn create_db_sql(db: &str) -> &'static str {
    match db {
        "detag_p083_abuse_ac" => "CREATE DATABASE detag_p083_abuse_ac",
        "detag_p083_abuse_flag_off" => "CREATE DATABASE detag_p083_abuse_flag_off",
        "detag_p083_abuse_quiet" => "CREATE DATABASE detag_p083_abuse_quiet",
        "detag_p083_abuse_dedup" => "CREATE DATABASE detag_p083_abuse_dedup",
        "detag_p083_abuse_old" => "CREATE DATABASE detag_p083_abuse_old",
        "detag_p083_abuse_suspend" => "CREATE DATABASE detag_p083_abuse_suspend",
        "detag_p083_abuse_disabled" => "CREATE DATABASE detag_p083_abuse_disabled",
        _ => panic!("unknown P1-083 test db {db}"),
    }
}

async fn setup(db: &str) -> DbPool {
    let url = database_url();
    let admin = connect(&url, &PoolConfig::default())
        .await
        .unwrap_or_else(|err| {
            panic!(
                "live P1-083 test needs Postgres on {url}: {err} — start it with `just dev` (or the native postgres, see DOCKER-001)"
            )
        });
    sqlx::query(drop_db_sql(db))
        .execute(&admin)
        .await
        .expect("drop stale test db");
    sqlx::query(create_db_sql(db))
        .execute(&admin)
        .await
        .unwrap_or_else(|err| panic!("live P1-083 test needs CREATEDB for the pf role: {err}"));
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
    let email = format!("abuse-{}@example.com", Uuid::now_v7());
    users::create(pool, &email, "argon2id$placeholder")
        .await
        .expect("seed user")
        .id
}

/// Seed `n` queued image jobs for the user (no upload: a job's
/// `upload_id` is NULLABLE, and the detector counts creations,
/// not uploads). The idempotency key is unique per job — the
/// table rejects a duplicate.
async fn seed_jobs(pool: &DbPool, user_id: Uuid, n: usize) {
    for i in 0..n {
        jobs::create(
            pool,
            &jobs::NewJob {
                user_id,
                upload_id: None,
                kind: "image",
                mode: "inpaint",
                status: "queued",
                lane: "standard",
                params: serde_json::json!({}),
                idempotency_key: Some(&format!("p083-{user_id}-{i}")),
                key_id: None,
            },
        )
        .await
        .expect("seed job");
    }
}

async fn report_count(pool: &DbPool, user_id: Uuid) -> i64 {
    let row = sqlx::query("SELECT COUNT(*) AS count FROM abuse_reports WHERE user_id = $1")
        .bind(user_id)
        .fetch_one(pool)
        .await
        .expect("count reports");
    row.try_get("count").expect("count column")
}

async fn spike_audit_count(pool: &DbPool) -> usize {
    audit_logs::list_for_action(pool, SPIKE_AUDIT_ACTION)
        .await
        .expect("list spike audits")
        .len()
}

async fn user_status(pool: &DbPool, user_id: Uuid) -> String {
    users::by_id(pool, user_id)
        .await
        .expect("user lookup")
        .expect("user must exist")
        .status
}

fn config(auto_suspend: bool) -> AbuseConfig {
    AbuseConfig {
        enabled: true,
        jobs_per_minute: 3,
        window_seconds: 60,
        auto_suspend,
    }
}

fn clock_now() -> Arc<ManualClock> {
    Arc::new(ManualClock::new(Utc::now()))
}

#[tokio::test]
async fn spike_detected() {
    let db = "detag_p083_abuse_ac";
    let pool = setup(db).await;
    let user_id = seed_user(&pool).await;
    seed_jobs(&pool, user_id, 3).await;

    let check = check_velocity(&pool, clock_now(), &config(false), user_id)
        .await
        .expect("check velocity");
    assert!(check.checked);
    assert!(check.detected, "3 jobs in 60 s must trip a threshold of 3");
    assert_eq!(check.jobs_in_window, 3);
    assert!(!check.already_reported);
    assert!(!check.suspended);
    let report_id = check.report_id.expect("a report must be filed");

    assert_eq!(report_count(&pool, user_id).await, 1, "exactly one report");
    let report = abuse_reports::by_id(&pool, report_id)
        .await
        .expect("read report")
        .expect("report must exist");
    assert_eq!(report.user_id, Some(user_id));
    assert_eq!(report.status, "open");
    assert_eq!(report.reason, "velocity: 3 jobs in 60s (threshold 3)");
    assert_eq!(
        report.job_id, None,
        "a spike is about the user, not one job"
    );

    assert_eq!(spike_audit_count(&pool).await, 1, "exactly one spike audit");
    let audits = audit_logs::list_for_action(&pool, SPIKE_AUDIT_ACTION)
        .await
        .expect("list spike audits");
    let audit = &audits[0];
    assert_eq!(
        audit.target_id.as_deref(),
        Some(user_id.to_string()).as_deref()
    );
    assert_eq!(audit.actor_client.as_deref(), Some("abuse-spike"));
    assert_eq!(
        audit.after.as_ref().unwrap()["report_id"],
        report_id.to_string()
    );
    assert_eq!(audit.after.as_ref().unwrap()["suspended"], false);

    teardown(pool, db).await;
}

#[tokio::test]
async fn flag_off_leaves_account_active() {
    let db = "detag_p083_abuse_flag_off";
    let pool = setup(db).await;
    let user_id = seed_user(&pool).await;
    seed_jobs(&pool, user_id, 3).await;

    let check = check_velocity(&pool, clock_now(), &config(false), user_id)
        .await
        .expect("check velocity");
    assert!(
        check.detected,
        "the spike must still be detected with the flag off"
    );
    assert!(!check.suspended, "flag off must never suspend");

    assert_eq!(
        user_status(&pool, user_id).await,
        "active",
        "flag off leaves the account active"
    );
    let suspensions = audit_logs::list_for_action(&pool, "user.suspended")
        .await
        .expect("list suspensions");
    assert!(
        suspensions.is_empty(),
        "flag off must write no user.suspended audit row"
    );

    teardown(pool, db).await;
}

#[tokio::test]
async fn under_threshold_is_quiet() {
    let db = "detag_p083_abuse_quiet";
    let pool = setup(db).await;
    let user_id = seed_user(&pool).await;
    seed_jobs(&pool, user_id, 2).await;

    let check = check_velocity(&pool, clock_now(), &config(false), user_id)
        .await
        .expect("check velocity");
    assert!(check.checked);
    assert!(!check.detected, "2 jobs must not trip a threshold of 3");
    assert_eq!(check.jobs_in_window, 2);
    assert_eq!(check.report_id, None);

    assert_eq!(
        report_count(&pool, user_id).await,
        0,
        "no report under threshold"
    );
    assert_eq!(
        spike_audit_count(&pool).await,
        0,
        "no audit under threshold"
    );

    teardown(pool, db).await;
}

#[tokio::test]
async fn second_check_in_window_does_not_duplicate() {
    let db = "detag_p083_abuse_dedup";
    let pool = setup(db).await;
    let user_id = seed_user(&pool).await;
    seed_jobs(&pool, user_id, 3).await;

    let first = check_velocity(&pool, clock_now(), &config(false), user_id)
        .await
        .expect("first check");
    assert!(first.detected);
    let report_id = first.report_id.expect("first check files");

    let second = check_velocity(&pool, clock_now(), &config(false), user_id)
        .await
        .expect("second check");
    assert!(
        second.detected,
        "the spike is still a spike on the second check"
    );
    assert!(
        second.already_reported,
        "the second check must be a dedup hit"
    );
    assert_eq!(
        second.report_id,
        Some(report_id),
        "the same report, not a new one"
    );
    assert!(!second.suspended);

    assert_eq!(
        report_count(&pool, user_id).await,
        1,
        "a sustained spike files once per window"
    );
    assert_eq!(
        spike_audit_count(&pool).await,
        1,
        "the dedup hit writes no second audit row"
    );

    teardown(pool, db).await;
}

#[tokio::test]
async fn old_jobs_outside_window_do_not_count() {
    let db = "detag_p083_abuse_old";
    let pool = setup(db).await;
    let user_id = seed_user(&pool).await;
    seed_jobs(&pool, user_id, 3).await;
    sqlx::query("UPDATE jobs SET created_at = now() - interval '10 minutes' WHERE user_id = $1")
        .bind(user_id)
        .execute(&pool)
        .await
        .expect("age the jobs");

    let check = check_velocity(&pool, clock_now(), &config(false), user_id)
        .await
        .expect("check velocity");
    assert!(
        !check.detected,
        "3 jobs 10 minutes ago must not trip a 60 s window"
    );
    assert_eq!(check.jobs_in_window, 0);
    assert_eq!(report_count(&pool, user_id).await, 0);

    teardown(pool, db).await;
}

#[tokio::test]
async fn auto_suspend_suspends_when_on() {
    let db = "detag_p083_abuse_suspend";
    let pool = setup(db).await;
    let user_id = seed_user(&pool).await;
    seed_jobs(&pool, user_id, 3).await;

    let check = check_velocity(&pool, clock_now(), &config(true), user_id)
        .await
        .expect("check velocity");
    assert!(check.detected);
    assert!(check.suspended, "flag on must suspend on a spike");
    assert!(check.report_id.is_some(), "flag on still files the report");

    assert_eq!(
        user_status(&pool, user_id).await,
        "suspended",
        "flag on suspends the account"
    );
    // The 3 seeded jobs were queued: the suspend holds all of them.
    let jobs = jobs::list_by_user(&pool, user_id, 10)
        .await
        .expect("list jobs");
    assert!(
        jobs.iter().all(|job| job.status == "held"),
        "suspend holds the queued jobs, got {:?}",
        jobs.iter().map(|job| &job.status).collect::<Vec<_>>()
    );
    let suspensions = audit_logs::list_for_action(&pool, "user.suspended")
        .await
        .expect("list suspensions");
    assert_eq!(suspensions.len(), 1, "exactly one suspension audit");
    assert_eq!(suspensions[0].actor_client.as_deref(), Some("abuse-spike"));
    assert_eq!(suspensions[0].after.as_ref().unwrap()["jobs_held"], 3);
    let audits = audit_logs::list_for_action(&pool, SPIKE_AUDIT_ACTION)
        .await
        .expect("list spike audits");
    assert_eq!(audits.len(), 1);
    assert_eq!(audits[0].after.as_ref().unwrap()["suspended"], true);

    teardown(pool, db).await;
}

#[tokio::test]
async fn disabled_detector_files_nothing() {
    let db = "detag_p083_abuse_disabled";
    let pool = setup(db).await;
    let user_id = seed_user(&pool).await;
    seed_jobs(&pool, user_id, 5).await;
    let disabled = AbuseConfig {
        enabled: false,
        ..config(false)
    };

    let check = check_velocity(&pool, clock_now(), &disabled, user_id)
        .await
        .expect("check velocity");
    assert!(!check.checked, "disabled means the check did not run");
    assert!(!check.detected);
    assert_eq!(check.jobs_in_window, 0);
    assert_eq!(report_count(&pool, user_id).await, 0);
    assert_eq!(spike_audit_count(&pool).await, 0);

    teardown(pool, db).await;
}
