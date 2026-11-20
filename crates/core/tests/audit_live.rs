//! Live audit-service tests (P1-087, E1.10, FR-C-03).
//!
//! The audit service (`detag_core::audit::record`) is the only
//! write path to `audit_logs` — the repository insert is
//! `pub(crate)` and a unit test in the service scans the
//! workspace for direct calls. These tests run against the real
//! services: a scratch Postgres DB per test (created + dropped
//! by the test, the `abuse_live.rs` pattern). No Redis leg:
//! the audit trail is a Postgres table.
//!
//! - `record_writes_full_event` — the ticket's AC shape: one
//!   `record` call writes a row carrying actor, action,
//!   target, before, after and reason, readable back.
//! - `record_rejects_invalid_events` — a malformed event
//!   (empty / undotted / whitespace action, empty target type)
//!   fails with `RepoError::InvalidInput` and writes nothing.
//! - `append_only_denied_on_service_written_row` — the
//!   ticket's AC: UPDATE and DELETE on a row the service wrote
//!   fail on the append-only trigger (the "db grant test" —
//!   enforcement is the trigger, not a GRANT, because the dev
//!   role owns the tables; see `audit.rs`).

use detag_core::audit::{record, NewAuditLog};
use detag_core::db::{connect, DbPool, PoolConfig};
use detag_core::migrate::migrate_up;
use detag_core::repos::{audit_logs, RepoError};
use serde_json::json;
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
        "detag_p087_audit_record" => "DROP DATABASE IF EXISTS detag_p087_audit_record",
        "detag_p087_audit_reject" => "DROP DATABASE IF EXISTS detag_p087_audit_reject",
        "detag_p087_audit_append_only" => "DROP DATABASE IF EXISTS detag_p087_audit_append_only",
        _ => panic!("unknown P1-087 test db {db}"),
    }
}

fn create_db_sql(db: &str) -> &'static str {
    match db {
        "detag_p087_audit_record" => "CREATE DATABASE detag_p087_audit_record",
        "detag_p087_audit_reject" => "CREATE DATABASE detag_p087_audit_reject",
        "detag_p087_audit_append_only" => "CREATE DATABASE detag_p087_audit_append_only",
        _ => panic!("unknown P1-087 test db {db}"),
    }
}

async fn setup(db: &str) -> DbPool {
    let url = database_url();
    let admin = connect(&url, &PoolConfig::default())
        .await
        .unwrap_or_else(|err| {
            panic!(
                "live P1-087 test needs Postgres on {url}: {err} — start it with `just dev` (or the native postgres, see DOCKER-001)"
            )
        });
    sqlx::query(drop_db_sql(db))
        .execute(&admin)
        .await
        .expect("drop stale test db");
    sqlx::query(create_db_sql(db))
        .execute(&admin)
        .await
        .unwrap_or_else(|err| panic!("live P1-087 test needs CREATEDB for the pf role: {err}"));
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

fn full_event(actor: Uuid) -> NewAuditLog {
    NewAuditLog {
        actor_user_id: Some(actor),
        actor_client: Some("web".to_string()),
        action: "user.suspended".to_string(),
        target_type: "user".to_string(),
        target_id: Some(actor.to_string()),
        before: Some(json!({ "status": "active" })),
        after: Some(json!({ "status": "suspended" })),
        reason: Some("velocity spike".to_string()),
    }
}

#[tokio::test]
async fn record_writes_full_event() {
    let db = "detag_p087_audit_record";
    let pool = setup(db).await;
    let actor = Uuid::now_v7();

    let id = record(&pool, &full_event(actor))
        .await
        .expect("record a full event");
    let rows = audit_logs::list_for_target(&pool, "user", &actor.to_string())
        .await
        .expect("read the event back");
    assert_eq!(rows.len(), 1, "exactly one row for the target");
    let row = &rows[0];
    assert_eq!(row.id, id);
    assert_eq!(row.actor_user_id, Some(actor));
    assert_eq!(row.actor_client.as_deref(), Some("web"));
    assert_eq!(row.action, "user.suspended");
    assert_eq!(row.target_type, "user");
    assert_eq!(row.target_id.as_deref(), Some(actor.to_string().as_str()));
    assert_eq!(row.before, Some(json!({ "status": "active" })));
    assert_eq!(row.after, Some(json!({ "status": "suspended" })));
    assert_eq!(row.reason.as_deref(), Some("velocity spike"));

    teardown(pool, db).await;
}

#[tokio::test]
async fn record_rejects_invalid_events() {
    let db = "detag_p087_audit_reject";
    let pool = setup(db).await;
    let actor = Uuid::now_v7();

    let mut bad_action = full_event(actor);
    bad_action.action = "suspended".to_string();
    let mut empty_action = full_event(actor);
    empty_action.action = String::new();
    let mut space_action = full_event(actor);
    space_action.action = "user. suspended".to_string();
    let mut empty_target = full_event(actor);
    empty_target.target_type = String::new();
    for event in [&bad_action, &empty_action, &space_action, &empty_target] {
        assert!(
            matches!(
                record(&pool, event).await,
                Err(RepoError::InvalidInput { .. })
            ),
            "malformed event must fail validation: {event:?}"
        );
    }
    // Nothing was written: the validation fails before any SQL.
    let rows = audit_logs::list_for_action(&pool, "user.suspended")
        .await
        .expect("read back");
    assert_eq!(rows.len(), 0, "no invalid event may land a row");

    teardown(pool, db).await;
}

#[tokio::test]
async fn append_only_denied_on_service_written_row() {
    let db = "detag_p087_audit_append_only";
    let pool = setup(db).await;
    let actor = Uuid::now_v7();

    record(&pool, &full_event(actor))
        .await
        .expect("record the event");
    for sql in [
        "UPDATE audit_logs SET action = 'tampered'",
        "DELETE FROM audit_logs",
    ] {
        let err = sqlx::query(sql).execute(&pool).await;
        assert!(err.is_err(), "{sql} on audit_logs must fail");
        assert!(
            err.expect_err("audit log mutation must fail")
                .to_string()
                .contains("append-only"),
            "audit log mutation must fail on the append-only trigger"
        );
    }
    // The row is still there, unmodified.
    let rows = audit_logs::list_for_target(&pool, "user", &actor.to_string())
        .await
        .expect("read back");
    assert_eq!(rows.len(), 1, "the row survives the denied mutations");
    assert_eq!(rows[0].action, "user.suspended");

    teardown(pool, db).await;
}
