//! Live attestation-service tests (P1-094, FR-C-01 / ADR-0012).
//!
//! The attestation service (`detag_core::attestation`) is the
//! only writer of `attestations` rows: capture validates the
//! inputs, proves the upload belongs to the caller, hashes the
//! IP (NFR-42) and inserts; the job-create gate reads through
//! `require_for_upload`. These tests run against the real
//! services: a scratch Postgres DB per test (created + dropped
//! by the test, the `abuse_live.rs` pattern). No Redis leg: the
//! attestation trail is a Postgres table. Row immutability
//! itself is proven by the migration-004 live tests
//! (`migration_004_audit_flags_misc.rs` — UPDATE / DELETE on
//! an attestation row fail on the trigger); this file proves
//! the service behavior on top of it.
//!
//! - `capture_roundtrip_stores_hash_not_ip` — capture writes
//!   a row carrying user, upload, version, the sha256 of the
//!   IP (never the address) and the client, readable back.
//! - `capture_rejects_empty_version_and_empty_ip` — an empty
//!   text version or an empty IP fails with `InvalidInput`
//!   and writes nothing.
//! - `capture_on_foreign_or_unknown_upload_is_not_found` —
//!   attesting for another user's upload, or for an upload
//!   that does not exist, is `UploadNotFound` (the P1-045
//!   IDOR rule: a foreign upload is a 404).
//! - `require_for_upload_missing_is_attestation_missing` —
//!   the gate's rejection: no row for (user, upload) is
//!   `AttestationMissing`, which maps to PF-4001 / 403.
//! - `require_for_upload_returns_the_newest_row` —
//!   re-attesting writes a second row (rows are immutable, so
//!   a correction is a fresh attestation) and the gate takes
//!   the newest one.

use detag_core::attestation::{
    capture, hash_ip, require_for_upload, AttestationError, Client, CURRENT_TEXT_VERSION,
};
use detag_core::db::{connect, DbPool, PoolConfig};
use detag_core::error::AppError;
use detag_core::migrate::migrate_up;
use detag_core::repos::{attestations, uploads, users};
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
        "detag_p094_attest_capture" => "DROP DATABASE IF EXISTS detag_p094_attest_capture",
        "detag_p094_attest_reject" => "DROP DATABASE IF EXISTS detag_p094_attest_reject",
        "detag_p094_attest_foreign" => "DROP DATABASE IF EXISTS detag_p094_attest_foreign",
        "detag_p094_attest_missing" => "DROP DATABASE IF EXISTS detag_p094_attest_missing",
        "detag_p094_attest_newest" => "DROP DATABASE IF EXISTS detag_p094_attest_newest",
        _ => panic!("unknown P1-094 test db {db}"),
    }
}

fn create_db_sql(db: &str) -> &'static str {
    match db {
        "detag_p094_attest_capture" => "CREATE DATABASE detag_p094_attest_capture",
        "detag_p094_attest_reject" => "CREATE DATABASE detag_p094_attest_reject",
        "detag_p094_attest_foreign" => "CREATE DATABASE detag_p094_attest_foreign",
        "detag_p094_attest_missing" => "CREATE DATABASE detag_p094_attest_missing",
        "detag_p094_attest_newest" => "CREATE DATABASE detag_p094_attest_newest",
        _ => panic!("unknown P1-094 test db {db}"),
    }
}

async fn setup(db: &str) -> DbPool {
    let url = database_url();
    let admin = connect(&url, &PoolConfig::default())
        .await
        .unwrap_or_else(|err| {
            panic!(
                "live P1-094 test needs Postgres on {url}: {err} — start it with `just dev` (or the native postgres, see DOCKER-001)"
            )
        });
    sqlx::query(drop_db_sql(db))
        .execute(&admin)
        .await
        .expect("drop stale test db");
    sqlx::query(create_db_sql(db))
        .execute(&admin)
        .await
        .unwrap_or_else(|err| panic!("live P1-094 test needs CREATEDB for the pf role: {err}"));
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
    let email = format!("attest-{}@example.com", Uuid::now_v7());
    users::create(pool, &email, "argon2id$placeholder")
        .await
        .expect("seed user")
        .id
}

async fn seed_upload(pool: &DbPool, user_id: Uuid) -> Uuid {
    uploads::create(
        pool,
        &uploads::NewUpload {
            user_id,
            kind: "image",
            filename: "a.png",
            bytes: 100,
            sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            mime: "image/png",
            storage_key: "test/attest-seed",
            parts: 1,
        },
    )
    .await
    .expect("seed upload")
    .id
}

/// The client IP the tests attest from (TEST-NET-3 — never a
/// real address; the stored value is its hash, never the IP).
const TEST_IP: &str = "203.0.113.7";

#[tokio::test]
async fn capture_roundtrip_stores_hash_not_ip() {
    let db = "detag_p094_attest_capture";
    let pool = setup(db).await;
    let user = seed_user(&pool).await;
    let upload = seed_upload(&pool, user).await;

    let row = capture(
        &pool,
        user,
        upload,
        CURRENT_TEXT_VERSION,
        TEST_IP,
        Client::Web,
    )
    .await
    .expect("capture an attestation");
    assert_eq!(row.user_id, user);
    assert_eq!(row.upload_id, upload);
    assert_eq!(row.text_version, CURRENT_TEXT_VERSION);
    assert_eq!(
        row.ip_hash,
        hash_ip(TEST_IP),
        "the stored hash is the sha256"
    );
    assert_ne!(row.ip_hash, TEST_IP, "the raw IP is never stored");
    assert_eq!(row.client, "web");

    let back = attestations::by_id(&pool, row.id)
        .await
        .expect("read the row back")
        .expect("captured row must exist");
    assert_eq!(back, row, "the row reads back exactly");

    teardown(pool, db).await;
}

#[tokio::test]
async fn capture_rejects_empty_version_and_empty_ip() {
    let db = "detag_p094_attest_reject";
    let pool = setup(db).await;
    let user = seed_user(&pool).await;
    let upload = seed_upload(&pool, user).await;

    for (version, ip) in [("", TEST_IP), (CURRENT_TEXT_VERSION, "")] {
        assert!(
            matches!(
                capture(&pool, user, upload, version, ip, Client::Web).await,
                Err(AttestationError::InvalidInput { .. })
            ),
            "empty version / empty IP must fail validation: version={version:?} ip={ip:?}"
        );
    }
    // Nothing was written: the validation fails before any SQL.
    let found = attestations::for_upload(&pool, user, upload)
        .await
        .expect("read back");
    assert!(found.is_none(), "no invalid capture may land a row");

    teardown(pool, db).await;
}

#[tokio::test]
async fn capture_on_foreign_or_unknown_upload_is_not_found() {
    let db = "detag_p094_attest_foreign";
    let pool = setup(db).await;
    let user = seed_user(&pool).await;
    let other = seed_user(&pool).await;
    let upload = seed_upload(&pool, other).await;

    // Another user's upload: a 404, never a leak that it exists.
    assert!(
        matches!(
            capture(
                &pool,
                user,
                upload,
                CURRENT_TEXT_VERSION,
                TEST_IP,
                Client::Web
            )
            .await,
            Err(AttestationError::UploadNotFound)
        ),
        "attesting for a foreign upload must be UploadNotFound"
    );
    // An upload that does not exist at all: the same 404.
    assert!(
        matches!(
            capture(
                &pool,
                user,
                Uuid::now_v7(),
                CURRENT_TEXT_VERSION,
                TEST_IP,
                Client::Web
            )
            .await,
            Err(AttestationError::UploadNotFound)
        ),
        "attesting for an unknown upload must be UploadNotFound"
    );
    assert_eq!(
        AppError::from(AttestationError::UploadNotFound).code(),
        "PF-2010"
    );

    teardown(pool, db).await;
}

#[tokio::test]
async fn require_for_upload_missing_is_attestation_missing() {
    let db = "detag_p094_attest_missing";
    let pool = setup(db).await;
    let user = seed_user(&pool).await;
    let upload = seed_upload(&pool, user).await;

    let err = require_for_upload(&pool, user, upload)
        .await
        .expect_err("no attestation row exists yet");
    assert!(
        matches!(err, AttestationError::AttestationMissing),
        "the gate's rejection is AttestationMissing: {err:?}"
    );
    assert_eq!(AppError::from(err).code(), "PF-4001");

    // Another user's attestation for the same upload does not
    // satisfy this user's gate — rows are scoped to the caller.
    let other = seed_user(&pool).await;
    let other_upload = seed_upload(&pool, other).await;
    capture(
        &pool,
        other,
        other_upload,
        CURRENT_TEXT_VERSION,
        TEST_IP,
        Client::ApiKey,
    )
    .await
    .expect("the other user attests for their own upload");
    assert!(
        matches!(
            require_for_upload(&pool, user, other_upload).await,
            Err(AttestationError::AttestationMissing)
        ),
        "another user's row must not satisfy this user's gate"
    );

    teardown(pool, db).await;
}

#[tokio::test]
async fn require_for_upload_returns_the_newest_row() {
    let db = "detag_p094_attest_newest";
    let pool = setup(db).await;
    let user = seed_user(&pool).await;
    let upload = seed_upload(&pool, user).await;

    capture(&pool, user, upload, "v1", TEST_IP, Client::Web)
        .await
        .expect("first attestation");
    // Re-attesting writes a second row — rows are immutable,
    // so a correction is a fresh attestation, never an edit.
    capture(&pool, user, upload, "v2", TEST_IP, Client::ApiKey)
        .await
        .expect("second attestation");

    let gate = require_for_upload(&pool, user, upload)
        .await
        .expect("the gate finds the newest row");
    assert_eq!(gate.text_version, "v2", "the gate takes the newest row");
    assert_eq!(gate.client, "api_key");

    teardown(pool, db).await;
}
