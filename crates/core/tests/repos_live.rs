//! Live repository tests (P0-041, E0.4).
//!
//! Exercises the `repos` layer against a real Postgres on
//! `PF_DATABASE_URL` (default: the dev database) using a scratch
//! DATABASE per test (created and dropped by the test) with all
//! repo migrations applied — never the dev database, so the dev
//! `_sqlx_migrations` is never touched. Covers repo roundtrips,
//! constraint violations mapped to typed `RepoError`s, JSONB
//! roundtrips, and pool behaviour under parallelism.
//!
//! Testcontainers is the kit's intended fixture, but docker cannot
//! start containers on this VM (DOCKER-001), so these tests use the
//! native postgres — the same pattern as the migration live tests.
//! The `pf` role needs CREATEDB (granted in the VM dev setup; CI's
//! postgres service user has it).

use chrono::{Duration, Utc};
use detag_core::db::{connect, DbPool, PoolConfig};
use detag_core::migrate::migrate_up;
use detag_core::repos::error::RepoError;
use detag_core::repos::{
    artifacts, email_outbox, email_outbox::NewOutboxMail, jobs, plans, uploads, users,
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
/// dynamically built SQL strings — so the DROP/CREATE statements
/// are literals selected by the (known, fixed) test DB name.
fn drop_db_sql(db: &str) -> &'static str {
    match db {
        "detag_p041_users" => "DROP DATABASE IF EXISTS detag_p041_users",
        "detag_p041_users_dup" => "DROP DATABASE IF EXISTS detag_p041_users_dup",
        "detag_p041_jobs" => "DROP DATABASE IF EXISTS detag_p041_jobs",
        "detag_p041_jobs_err" => "DROP DATABASE IF EXISTS detag_p041_jobs_err",
        "detag_p041_plans" => "DROP DATABASE IF EXISTS detag_p041_plans",
        "detag_p041_pool" => "DROP DATABASE IF EXISTS detag_p041_pool",
        "detag_p057_uploads" => "DROP DATABASE IF EXISTS detag_p057_uploads",
        "detag_p057_uploads_guarded" => "DROP DATABASE IF EXISTS detag_p057_uploads_guarded",
        "detag_p058_uploads_hash" => "DROP DATABASE IF EXISTS detag_p058_uploads_hash",
        "detag_p060_artifacts" => "DROP DATABASE IF EXISTS detag_p060_artifacts",
        "detag_p128_outbox_claim" => "DROP DATABASE IF EXISTS detag_p128_outbox_claim",
        "detag_p128_outbox_max" => "DROP DATABASE IF EXISTS detag_p128_outbox_max",
        "detag_p128_outbox_truncate" => "DROP DATABASE IF EXISTS detag_p128_outbox_truncate",
        "detag_p128_outbox_race" => "DROP DATABASE IF EXISTS detag_p128_outbox_race",
        "detag_p131_outbox_rate_count" => "DROP DATABASE IF EXISTS detag_p131_outbox_rate_count",
        _ => panic!("unknown P0-041 test db {db}"),
    }
}

fn create_db_sql(db: &str) -> &'static str {
    match db {
        "detag_p041_users" => "CREATE DATABASE detag_p041_users",
        "detag_p041_users_dup" => "CREATE DATABASE detag_p041_users_dup",
        "detag_p041_jobs" => "CREATE DATABASE detag_p041_jobs",
        "detag_p041_jobs_err" => "CREATE DATABASE detag_p041_jobs_err",
        "detag_p041_plans" => "CREATE DATABASE detag_p041_plans",
        "detag_p041_pool" => "CREATE DATABASE detag_p041_pool",
        "detag_p057_uploads" => "CREATE DATABASE detag_p057_uploads",
        "detag_p057_uploads_guarded" => "CREATE DATABASE detag_p057_uploads_guarded",
        "detag_p058_uploads_hash" => "CREATE DATABASE detag_p058_uploads_hash",
        "detag_p060_artifacts" => "CREATE DATABASE detag_p060_artifacts",
        "detag_p128_outbox_claim" => "CREATE DATABASE detag_p128_outbox_claim",
        "detag_p128_outbox_max" => "CREATE DATABASE detag_p128_outbox_max",
        "detag_p128_outbox_truncate" => "CREATE DATABASE detag_p128_outbox_truncate",
        "detag_p128_outbox_race" => "CREATE DATABASE detag_p128_outbox_race",
        "detag_p131_outbox_rate_count" => "CREATE DATABASE detag_p131_outbox_rate_count",
        _ => panic!("unknown P0-041 test db {db}"),
    }
}

async fn setup(db: &str) -> DbPool {
    let url = database_url();
    let admin = connect(&url, &PoolConfig::default())
        .await
        .unwrap_or_else(|err| {
            panic!(
                "live P0-041 test needs Postgres on {url}: {err} — start it with `just dev` (or the native postgres, see DOCKER-001)"
            )
        });
    sqlx::query(drop_db_sql(db))
        .execute(&admin)
        .await
        .expect("drop stale test db");
    sqlx::query(create_db_sql(db))
        .execute(&admin)
        .await
        .unwrap_or_else(|err| panic!("live P0-041 test needs CREATEDB for the pf role: {err}"));
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

#[tokio::test]
async fn users_roundtrip_and_case_insensitive_lookup() {
    let db = "detag_p041_users";
    let pool = setup(db).await;

    let created = users::create(&pool, "Carol@Example.com", "argon2id$placeholder")
        .await
        .expect("create user");
    assert_eq!(created.email, "Carol@Example.com");
    assert_eq!(created.role, "user");
    assert_eq!(created.status, "active");
    assert_eq!(created.settings, serde_json::json!({}));
    assert!(created.email_verified_at.is_none());

    let by_id = users::by_id(&pool, created.id)
        .await
        .expect("by_id")
        .expect("user must exist");
    assert_eq!(by_id, created, "roundtrip must preserve every field");

    // citext: a different case still finds the same row.
    let by_email = users::by_email(&pool, "carol@example.com")
        .await
        .expect("by_email")
        .expect("case-insensitive lookup must find the user");
    assert_eq!(by_email.id, created.id);

    assert!(users::by_id(&pool, Uuid::nil())
        .await
        .expect("by_id")
        .is_none());
    assert!(users::by_email(&pool, "nobody@example.com")
        .await
        .expect("by_email")
        .is_none());

    teardown(pool, db).await;
}

/// P1-028: enqueue one outbox row for the drain tests below.
async fn enqueue_outbox(
    pool: &DbPool,
    template: &str,
    scheduled_at: Option<chrono::DateTime<Utc>>,
) -> Uuid {
    email_outbox::enqueue(
        pool,
        &NewOutboxMail {
            to_hash: "deadbeef".to_string(),
            template: template.to_string(),
            payload: serde_json::json!({ "user_id": Uuid::now_v7() }),
            scheduled_at,
        },
    )
    .await
    .expect("enqueue outbox row")
}

#[tokio::test]
async fn outbox_claim_due_claims_oldest_first_and_bumps_attempts() {
    let db = "detag_p128_outbox_claim";
    let pool = setup(db).await;
    let now = Utc::now();

    // Two due rows (A older than C) + one scheduled an hour out.
    let a = enqueue_outbox(&pool, "verify-email", Some(now - Duration::minutes(10))).await;
    let _b = enqueue_outbox(&pool, "verify-email", Some(now + Duration::hours(1))).await;
    let c = enqueue_outbox(&pool, "password-reset", Some(now - Duration::minutes(5))).await;

    let claimed = email_outbox::claim_due(&pool, 10, 5, Utc::now())
        .await
        .expect("claim due");
    assert_eq!(
        claimed.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![a, c],
        "oldest due first; the future row is not due"
    );
    assert!(
        claimed.iter().all(|r| r.attempts == 1),
        "the claim bumps attempts to 1"
    );

    // The sent row is never claimed again; the failed row is
    // rescheduled into the future and stays unclaimed until then.
    email_outbox::mark_sent(&pool, a).await.expect("mark sent");
    email_outbox::record_failure(&pool, c, "relay down", now + Duration::seconds(60))
        .await
        .expect("record failure");
    let claimed = email_outbox::claim_due(&pool, 10, 5, Utc::now())
        .await
        .expect("second claim");
    assert!(
        claimed.is_empty(),
        "sent + rescheduled rows are not due: got {}",
        claimed.len()
    );

    teardown(pool, db).await;
}

#[tokio::test]
async fn outbox_claim_stops_at_max_attempts_and_park_is_terminal() {
    let db = "detag_p128_outbox_max";
    let pool = setup(db).await;

    let one = enqueue_outbox(&pool, "verify-email", None).await;
    let two = enqueue_outbox(&pool, "verify-email", None).await;

    // max_attempts = 1: one claim spends the row's only attempt.
    let claimed = email_outbox::claim_due(&pool, 10, 1, Utc::now())
        .await
        .expect("claim");
    assert_eq!(claimed.len(), 2, "both rows claimable at first");
    let claimed = email_outbox::claim_due(&pool, 10, 1, Utc::now())
        .await
        .expect("second claim");
    assert!(
        claimed.is_empty(),
        "attempts == max means terminal, got {}",
        claimed.len()
    );

    // Parking is the same terminal state by construction.
    let _ = one;
    email_outbox::record_permanent_failure(&pool, two, 5, "unknown template")
        .await
        .expect("park");
    let rows = email_outbox::unsent_for_template(&pool, "verify-email")
        .await
        .expect("read back");
    let parked = rows
        .iter()
        .find(|r| r.id == two)
        .expect("parked row reads back");
    assert_eq!(parked.attempts, 5, "parking sets attempts to max");
    assert_eq!(parked.last_error.as_deref(), Some("unknown template"));

    teardown(pool, db).await;
}

/// P1-031: the sent-count for the drain's per-recipient cap
/// counts only SENT rows, only for the named templates, only
/// for the named recipient hash, and only inside the window.
#[tokio::test]
async fn outbox_count_sent_recent_for_recipient_counts_only_sent_in_window() {
    let db = "detag_p131_outbox_rate_count";
    let pool = setup(db).await;
    let now = Utc::now();
    let hash = "deadbeef-rate";
    let other_hash = "deadbeef-other";

    // One recent verify send (counts) + one verify send 61 min
    // ago (outside the window) + one unsent verify row (never
    // sent) + one recent reset send (other template) + one
    // recent change-verify send (same class) + one recent
    // verify send to another recipient.
    let recent = enqueue_outbox(&pool, "verify-email", None).await;
    let old = enqueue_outbox(&pool, "verify-email", None).await;
    let _unsent = enqueue_outbox(&pool, "verify-email", None).await;
    let reset = enqueue_outbox(&pool, "password-reset", None).await;
    let change = enqueue_outbox(&pool, "email-change-verify", None).await;
    let _other = enqueue_outbox(&pool, "verify-email", None).await;
    // The helper enqueues with a fixed to_hash; re-point the
    // rows at the two recipients under test.
    for (id, to_hash) in [
        (recent, hash),
        (old, hash),
        (reset, hash),
        (change, hash),
        (_other, other_hash),
    ] {
        sqlx::query("UPDATE email_outbox SET to_hash = $2 WHERE id = $1")
            .bind(id)
            .bind(to_hash)
            .execute(&pool)
            .await
            .expect("re-point to_hash");
    }
    for id in [recent, old, reset, change, _other] {
        email_outbox::mark_sent(&pool, id).await.expect("mark sent");
    }
    sqlx::query("UPDATE email_outbox SET sent_at = $2 WHERE id = $1")
        .bind(old)
        .bind(now - Duration::minutes(61))
        .execute(&pool)
        .await
        .expect("backdate old send");

    let verify_templates = ["verify-email", "email-change-verify"];
    let count = email_outbox::count_sent_recent_for_recipient(
        &pool,
        &verify_templates,
        hash,
        3600,
        Utc::now(),
    )
    .await
    .expect("count");
    assert_eq!(
        count, 2,
        "recent verify + recent change-verify only: old is out of window, unsent never sent, reset is another template"
    );
    let count = email_outbox::count_sent_recent_for_recipient(
        &pool,
        &["password-reset"],
        hash,
        3600,
        Utc::now(),
    )
    .await
    .expect("count reset");
    assert_eq!(count, 1, "the reset send counts in its own class");
    let count = email_outbox::count_sent_recent_for_recipient(
        &pool,
        &verify_templates,
        other_hash,
        3600,
        Utc::now(),
    )
    .await
    .expect("count other recipient");
    assert_eq!(count, 1, "another recipient has its own budget");
    let count = email_outbox::count_sent_recent_for_recipient(&pool, &[], hash, 3600, Utc::now())
        .await
        .expect("count empty set");
    assert_eq!(count, 0, "no templates means nothing to count");

    teardown(pool, db).await;
}

#[tokio::test]
async fn outbox_record_failure_truncates_long_errors() {
    let db = "detag_p128_outbox_truncate";
    let pool = setup(db).await;
    let now = Utc::now();

    let id = enqueue_outbox(&pool, "verify-email", None).await;
    let long = "x".repeat(600);
    email_outbox::record_failure(&pool, id, &long, now + Duration::seconds(60))
        .await
        .expect("record failure");
    let rows = email_outbox::unsent_for_template(&pool, "verify-email")
        .await
        .expect("read back");
    let row = rows.iter().find(|r| r.id == id).expect("row reads back");
    let kept = row.last_error.clone().expect("error kept");
    assert!(
        kept.chars().count() <= 501,
        "last_error must be truncated to 500 chars + ellipsis, got {}",
        kept.chars().count()
    );
    assert!(kept.ends_with('…'), "truncation is marked: {kept}");

    teardown(pool, db).await;
}

#[tokio::test]
async fn outbox_concurrent_claims_never_double_claim() {
    let db = "detag_p128_outbox_race";
    let pool = setup(db).await;

    for _ in 0..10 {
        enqueue_outbox(&pool, "verify-email", None).await;
    }
    // Four claimers race for the same 10 rows: SKIP LOCKED must
    // give each row to exactly one claimer.
    let mut handles = Vec::new();
    for _ in 0..4 {
        let pool = pool.clone();
        handles.push(tokio::spawn(async move {
            email_outbox::claim_due(&pool, 10, 5, Utc::now())
                .await
                .expect("concurrent claim")
        }));
    }
    let mut ids = Vec::new();
    for handle in handles {
        ids.extend(
            handle
                .await
                .expect("claimer joins")
                .into_iter()
                .map(|r| r.id),
        );
    }
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 10, "every row claimed exactly once");

    teardown(pool, db).await;
}

#[tokio::test]
async fn users_duplicate_email_is_typed_unique_violation() {
    let db = "detag_p041_users_dup";
    let pool = setup(db).await;

    users::create(&pool, "dup@example.com", "h")
        .await
        .expect("first create");
    let err = users::create(&pool, "DUP@example.com", "h")
        .await
        .expect_err("second create with the same email, different case, must fail");
    match err {
        RepoError::UniqueViolation { constraint } => {
            assert_eq!(constraint.as_deref(), Some("users_email_key"));
        }
        other => panic!("expected UniqueViolation, got {other:?}"),
    }

    teardown(pool, db).await;
}

#[tokio::test]
async fn jobs_roundtrip_jsonb_and_set_status() {
    let db = "detag_p041_jobs";
    let pool = setup(db).await;

    let user = users::create(&pool, "jobs@example.com", "h")
        .await
        .expect("create user");
    let params = serde_json::json!({
        "engine": {"mode": "inpaint", "strength": 0.8},
        "masks": [1, 2, 3],
        "seed": "abc",
    });
    let created = jobs::create(
        &pool,
        &jobs::NewJob {
            user_id: user.id,
            upload_id: None,
            kind: "image",
            mode: "inpaint",
            status: "queued",
            lane: "standard",
            params: params.clone(),
            idempotency_key: Some("idem-p041"),
        },
    )
    .await
    .expect("create job");
    assert_eq!(created.params, params, "params JSONB must roundtrip");
    assert_eq!(created.progress, 0.0);
    assert_eq!(created.attempts, 0);
    assert_eq!(created.idempotency_key.as_deref(), Some("idem-p041"));

    let by_id = jobs::by_id(&pool, created.id)
        .await
        .expect("by_id")
        .expect("job must exist");
    assert_eq!(by_id, created, "roundtrip must preserve every field");

    let listed = jobs::list_by_user(&pool, user.id, 10)
        .await
        .expect("list_by_user");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, created.id);

    let updated = jobs::set_status(&pool, created.id, "running")
        .await
        .expect("set_status");
    assert_eq!(updated.status, "running");
    assert_eq!(updated.id, created.id);

    let err = jobs::set_status(&pool, Uuid::nil(), "running")
        .await
        .expect_err("set_status on an unknown id must fail");
    assert!(
        matches!(err, RepoError::NotFound),
        "expected NotFound, got {err:?}"
    );

    teardown(pool, db).await;
}

#[tokio::test]
async fn jobs_constraint_violations_are_typed() {
    let db = "detag_p041_jobs_err";
    let pool = setup(db).await;

    let user = users::create(&pool, "jobserr@example.com", "h")
        .await
        .expect("create user");
    let base = jobs::NewJob {
        user_id: user.id,
        upload_id: None,
        kind: "image",
        mode: "inpaint",
        status: "queued",
        lane: "standard",
        params: serde_json::json!({}),
        idempotency_key: Some("idem-p041-err"),
    };
    jobs::create(&pool, &base).await.expect("first create");

    // Duplicate idempotency_key -> UniqueViolation.
    let err = jobs::create(&pool, &base)
        .await
        .expect_err("duplicate idempotency_key must fail");
    match err {
        RepoError::UniqueViolation { constraint } => {
            assert_eq!(constraint.as_deref(), Some("jobs_idempotency_key_key"));
        }
        other => panic!("expected UniqueViolation, got {other:?}"),
    }

    // Bad kind -> CheckViolation.
    let err = jobs::create(
        &pool,
        &jobs::NewJob {
            kind: "audio",
            idempotency_key: None,
            ..base.clone()
        },
    )
    .await
    .expect_err("bad kind must fail");
    match err {
        RepoError::CheckViolation { constraint } => {
            assert_eq!(constraint.as_deref(), Some("jobs_kind_check"));
        }
        other => panic!("expected CheckViolation, got {other:?}"),
    }

    // Unknown user -> ForeignKeyViolation.
    let err = jobs::create(
        &pool,
        &jobs::NewJob {
            user_id: Uuid::nil(),
            idempotency_key: None,
            ..base.clone()
        },
    )
    .await
    .expect_err("unknown user must fail");
    match err {
        RepoError::ForeignKeyViolation { constraint } => {
            assert_eq!(constraint.as_deref(), Some("jobs_user_id_fkey"));
        }
        other => panic!("expected ForeignKeyViolation, got {other:?}"),
    }

    teardown(pool, db).await;
}

#[tokio::test]
async fn plans_roundtrip_and_jsonb() {
    let db = "detag_p041_plans";
    let pool = setup(db).await;

    let pro = plans::by_code(&pool, "pro")
        .await
        .expect("by_code")
        .expect("pro plan must exist");
    assert_eq!(pro.amount, 49900);
    assert_eq!(pro.currency, "INR");
    assert_eq!(pro.entitlements["daily_images"], 500);
    assert_eq!(pro.entitlements["features"]["webhooks"], true);

    let all = plans::all(&pool).await.expect("all plans");
    assert_eq!(all.len(), 3);
    assert_eq!(all[0].code, "free");
    assert_eq!(all[1].code, "pro");
    assert_eq!(all[2].code, "business");
    assert_eq!(all[2].entitlements["lane"], "priority");

    assert!(plans::by_code(&pool, "enterprise")
        .await
        .expect("by_code")
        .is_none());

    teardown(pool, db).await;
}

#[tokio::test]
async fn uploads_roundtrip_and_constraints() {
    let db = "detag_p057_uploads";
    let pool = setup(db).await;

    let user = users::create(&pool, "uploads@example.com", "h")
        .await
        .expect("create user");
    let created = uploads::create(
        &pool,
        &uploads::NewUpload {
            user_id: user.id,
            kind: "image",
            filename: "input.png",
            bytes: 300,
            sha256: "abc123",
            mime: "image/png",
            storage_key: "dev/2026-11/019aa1b2-c3d4-7e5f-8000-000000000001/input.png",
            parts: 2,
        },
    )
    .await
    .expect("create upload");
    // Birth state is always pending_parts; the rest defaults.
    assert_eq!(created.status, "pending_parts");
    assert_eq!(created.parts, 2);
    assert_eq!(created.bytes, 300);
    assert_eq!(created.metadata, serde_json::json!({}));
    assert!(created.phash.is_none());
    assert!(created.width.is_none());

    let by_id = uploads::by_id(&pool, created.id)
        .await
        .expect("by_id")
        .expect("upload must exist");
    assert_eq!(by_id, created, "roundtrip must preserve every field");
    assert!(uploads::by_id(&pool, Uuid::nil())
        .await
        .expect("by_id")
        .is_none());

    // Bad kind -> CheckViolation.
    let base = uploads::NewUpload {
        user_id: user.id,
        kind: "image",
        filename: "input.png",
        bytes: 300,
        sha256: "abc123",
        mime: "image/png",
        storage_key: "dev/2026-11/x/input.png",
        parts: 1,
    };
    let err = uploads::create(
        &pool,
        &uploads::NewUpload {
            kind: "audio",
            ..base.clone()
        },
    )
    .await
    .expect_err("bad kind must fail");
    match err {
        RepoError::CheckViolation { constraint } => {
            assert_eq!(constraint.as_deref(), Some("uploads_kind_check"));
        }
        other => panic!("expected CheckViolation, got {other:?}"),
    }

    // Unknown user -> ForeignKeyViolation.
    let err = uploads::create(
        &pool,
        &uploads::NewUpload {
            user_id: Uuid::nil(),
            ..base.clone()
        },
    )
    .await
    .expect_err("unknown user must fail");
    match err {
        RepoError::ForeignKeyViolation { constraint } => {
            assert_eq!(constraint.as_deref(), Some("uploads_user_id_fkey"));
        }
        other => panic!("expected ForeignKeyViolation, got {other:?}"),
    }

    teardown(pool, db).await;
}

#[tokio::test]
async fn uploads_transitions_are_guarded() {
    let db = "detag_p057_uploads_guarded";
    let pool = setup(db).await;

    let user = users::create(&pool, "guarded@example.com", "h")
        .await
        .expect("create user");
    fn mk(user_id: Uuid, storage_key: &str) -> uploads::NewUpload<'_> {
        uploads::NewUpload {
            user_id,
            kind: "image",
            filename: "input.png",
            bytes: 300,
            sha256: "abc123",
            mime: "image/png",
            storage_key,
            parts: 1,
        }
    }

    let first = uploads::create(&pool, &mk(user.id, "dev/2026-11/a/input.png"))
        .await
        .expect("create upload");
    // pending_parts -> ready: the guard matches.
    let ready = uploads::set_status_guarded(&pool, first.id, "pending_parts", "ready")
        .await
        .expect("guarded transition")
        .expect("guard must match a pending upload");
    assert_eq!(ready.status, "ready");
    assert_eq!(ready.id, first.id);
    // A second complete is a no-op: the row is no longer pending.
    assert!(
        uploads::set_status_guarded(&pool, first.id, "pending_parts", "ready")
            .await
            .expect("second transition")
            .is_none(),
        "completing a ready upload must match nothing"
    );
    // Abort from ready is a no-op too.
    assert!(
        uploads::set_status_guarded(&pool, first.id, "pending_parts", "aborted")
            .await
            .expect("abort from ready")
            .is_none()
    );
    // Unknown id matches nothing.
    assert!(
        uploads::set_status_guarded(&pool, Uuid::nil(), "pending_parts", "ready")
            .await
            .expect("unknown id")
            .is_none()
    );

    // Abort path: pending_parts -> aborted, then terminal.
    let second = uploads::create(&pool, &mk(user.id, "dev/2026-11/b/input.png"))
        .await
        .expect("create second upload");
    let aborted = uploads::set_status_guarded(&pool, second.id, "pending_parts", "aborted")
        .await
        .expect("guarded abort")
        .expect("guard must match a pending upload");
    assert_eq!(aborted.status, "aborted");
    assert!(
        uploads::set_status_guarded(&pool, second.id, "pending_parts", "ready")
            .await
            .expect("ready from aborted")
            .is_none(),
        "completing an aborted upload must match nothing"
    );

    teardown(pool, db).await;
}

#[tokio::test]
async fn uploads_verified_hash_is_stored() {
    let db = "detag_p058_uploads_hash";
    let pool = setup(db).await;

    let user = users::create(&pool, "hash@example.com", "h")
        .await
        .expect("create user");
    // The creation-time claim (what the client reported) — here
    // deliberately wrong, so the test proves the stored value is
    // replaced by the server-computed one.
    let claim = "0000000000000000000000000000000000000000000000000000000000000000";
    let created = uploads::create(
        &pool,
        &uploads::NewUpload {
            user_id: user.id,
            kind: "image",
            filename: "input.png",
            bytes: 3,
            sha256: claim,
            mime: "image/png",
            storage_key: "dev/2026-11/c/input.png",
            parts: 1,
        },
    )
    .await
    .expect("create upload");
    assert_eq!(created.sha256, claim);

    // The server-computed hash (here: sha256("abc")) replaces the
    // claim while the upload is still pending.
    let verified = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    let stored = uploads::set_sha256(&pool, created.id, verified)
        .await
        .expect("set_sha256")
        .expect("guard must match a pending upload");
    assert_eq!(stored.sha256, verified);
    let by_id = uploads::by_id(&pool, created.id)
        .await
        .expect("by_id")
        .expect("upload must exist");
    assert_eq!(by_id.sha256, verified);

    // Once ready, the hash can no longer be overwritten.
    uploads::set_status_guarded(&pool, created.id, "pending_parts", "ready")
        .await
        .expect("mark ready")
        .expect("guard must match");
    assert!(
        uploads::set_sha256(
            &pool,
            created.id,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        )
        .await
        .expect("set_sha256 after ready")
        .is_none(),
        "storing a hash on a ready upload must match nothing"
    );
    assert!(uploads::set_sha256(&pool, Uuid::nil(), verified)
        .await
        .expect("set_sha256 unknown id")
        .is_none());

    teardown(pool, db).await;
}

#[tokio::test]
async fn pool_limits_respected_under_parallelism() {
    let db = "detag_p041_pool";
    let url = database_url();
    let admin = connect(&url, &PoolConfig::default())
        .await
        .expect("connect for setup");
    sqlx::query("DROP DATABASE IF EXISTS detag_p041_pool")
        .execute(&admin)
        .await
        .expect("drop stale test db");
    sqlx::query("CREATE DATABASE detag_p041_pool")
        .execute(&admin)
        .await
        .expect("create test db");
    admin.close().await;

    // Two connections only; ten concurrent lookups must all succeed
    // (queued by the pool, not failed with a pool timeout).
    let config = PoolConfig {
        max_connections: 2,
        ..PoolConfig::default()
    };
    let pool = connect(&test_database_url(db), &config)
        .await
        .expect("connect to the scratch database");
    migrate_up(&pool, &migrations_dir())
        .await
        .expect("migrate up");

    let mut set = tokio::task::JoinSet::new();
    for _ in 0..10 {
        let pool = pool.clone();
        set.spawn(async move { plans::by_code(&pool, "free").await });
    }
    let mut ok = 0;
    while let Some(res) = set.join_next().await {
        let plan = res.expect("task panicked").expect("by_code");
        assert!(plan.is_some(), "free plan must exist");
        ok += 1;
    }
    assert_eq!(ok, 10, "all ten concurrent lookups must succeed");

    teardown(pool, db).await;
}

#[tokio::test]
async fn artifacts_gc_key_lookups() {
    // P0-060: the GC's two queries — expired artifact keys, and
    // the full known-key set (artifacts + upload inputs).
    let db = "detag_p060_artifacts";
    let pool = setup(db).await;

    let user = users::create(&pool, "gc@example.com", "h")
        .await
        .expect("create user");
    let job = jobs::create(
        &pool,
        &jobs::NewJob {
            user_id: user.id,
            upload_id: None,
            kind: "image",
            mode: "inpaint",
            status: "succeeded",
            lane: "standard",
            params: serde_json::json!({}),
            idempotency_key: None,
        },
    )
    .await
    .expect("create job");
    // One upload row (the input key is "known" but never expires).
    let upload = uploads::create(
        &pool,
        &uploads::NewUpload {
            user_id: user.id,
            kind: "image",
            filename: "in.png",
            bytes: 10,
            sha256: "aa",
            mime: "image/png",
            storage_key: "dev/2026-11/job-1/input.png",
            parts: 1,
        },
    )
    .await
    .expect("create upload");
    let _ = upload;
    // Two artifacts: one expired an hour ago, one expiring tomorrow.
    // (No artifacts repo insert yet — the GC only reads; the fixture
    // inserts directly.)
    let now = Utc::now();
    for (key, artifact_type, expires_at) in [
        (
            "dev/2026-11/job-1/output.png",
            "output",
            now - chrono::Duration::hours(1),
        ),
        (
            "dev/2026-11/job-1/report.json",
            "report",
            now + chrono::Duration::hours(24),
        ),
    ] {
        sqlx::query(
            "INSERT INTO artifacts (job_id, type, storage_key, bytes, sha256, expires_at) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(job.id)
        .bind(artifact_type)
        .bind(key)
        .bind(10_i64)
        .bind("bb")
        .bind(expires_at)
        .execute(&pool)
        .await
        .expect("insert artifact");
    }

    let expired = artifacts::expired_storage_keys(&pool, now)
        .await
        .expect("expired_storage_keys");
    assert_eq!(expired, vec!["dev/2026-11/job-1/output.png"]);

    let mut known = artifacts::all_storage_keys(&pool)
        .await
        .expect("all_storage_keys");
    known.sort();
    assert_eq!(
        known,
        vec![
            "dev/2026-11/job-1/input.png",
            "dev/2026-11/job-1/output.png",
            "dev/2026-11/job-1/report.json",
        ]
    );

    teardown(pool, db).await;
}
