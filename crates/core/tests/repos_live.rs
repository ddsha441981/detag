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

use detag_core::db::{connect, DbPool, PoolConfig};
use detag_core::migrate::migrate_up;
use detag_core::repos::error::RepoError;
use detag_core::repos::{jobs, plans, uploads, users};
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
