//! Attestation repository (P1-094, FR-C-01 / ADR-0012).
//!
//! `attestations` is the ownership-attestation table (migration
//! 004): one row per attestation, `user_id` + `upload_id` +
//! `text_version` + `ip_hash` (sha256 of the client IP, NFR-42 —
//! the raw address is never stored) + `client` (`web` |
//! `api_key`). Rows are immutable: the migration's trigger
//! rejects UPDATE and DELETE (the purge cascade's erasure path
//! is the only DELETE — see the migration-004 live tests, which
//! prove the trigger; this module proves the service behavior).
//! The job-create gate reads through [`for_upload`]; the
//! capture service (`detag_core::attestation`) is the only
//! writer, and it checks the upload belongs to the caller
//! first (the P1-045 IDOR rule).

use super::error::RepoError;
use chrono::{DateTime, Utc};
use uuid::Uuid;

/// One attestation row (the fields the service and its tests
/// need). `client` is the stored `web` | `api_key` string —
/// the service's `Client` enum is the typed form.
#[derive(Debug, Clone, PartialEq)]
pub struct Attestation {
    pub id: Uuid,
    pub user_id: Uuid,
    pub upload_id: Uuid,
    pub text_version: String,
    pub ip_hash: String,
    pub client: String,
    pub created_at: DateTime<Utc>,
}

/// Insert an attestation row. The caller hashes the IP first
/// (the service does — this module never sees a raw address)
/// and validates the version and the upload ownership first.
/// A bad `client` fails with `RepoError::CheckViolation` (the
/// migration's `CHECK (client IN ('web', 'api_key'))`); an
/// unknown user or upload with `ForeignKeyViolation`.
pub async fn insert(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    user_id: Uuid,
    upload_id: Uuid,
    text_version: &str,
    ip_hash: &str,
    client: &str,
) -> Result<Attestation, RepoError> {
    sqlx::query_as!(
        Attestation,
        r#"INSERT INTO attestations (user_id, upload_id, text_version, ip_hash, client)
           VALUES ($1, $2, $3, $4, $5)
           RETURNING id, user_id, upload_id, text_version, ip_hash, client,
                     created_at"#,
        user_id,
        upload_id,
        text_version,
        ip_hash,
        client,
    )
    .fetch_one(pool)
    .await
    .map_err(RepoError::from_sqlx)
}

/// The user's newest attestation for an upload (`None` when
/// the user never attested for it). Newest first: an upload
/// can be re-attested (a new row — rows are immutable, so a
/// correction is a fresh attestation, never an edit), and the
/// job-create gate takes the latest one. Scoped to the
/// caller's rows by construction — another user's attestation
/// for the same upload is invisible here.
pub async fn for_upload(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    user_id: Uuid,
    upload_id: Uuid,
) -> Result<Option<Attestation>, RepoError> {
    sqlx::query_as!(
        Attestation,
        r#"SELECT id, user_id, upload_id, text_version, ip_hash, client,
                  created_at
           FROM attestations
           WHERE user_id = $1 AND upload_id = $2
           ORDER BY created_at DESC, id DESC
           LIMIT 1"#,
        user_id,
        upload_id,
    )
    .fetch_optional(pool)
    .await
    .map_err(RepoError::from_sqlx)
}

/// Read one attestation back by id (tests + the P1-098 admin
/// review queue). `None` when the id does not exist.
pub async fn by_id(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    id: Uuid,
) -> Result<Option<Attestation>, RepoError> {
    sqlx::query_as!(
        Attestation,
        r#"SELECT id, user_id, upload_id, text_version, ip_hash, client,
                  created_at
           FROM attestations
           WHERE id = $1"#,
        id,
    )
    .fetch_optional(pool)
    .await
    .map_err(RepoError::from_sqlx)
}
