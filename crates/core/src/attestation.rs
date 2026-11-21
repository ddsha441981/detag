//! Ownership attestation service (P1-094, FR-C-01 / ADR-0012).
//!
//! Every job creation requires an explicit ownership
//! attestation — "I own or have rights to this content, and
//! any watermark/mark originates from my own generation or
//! licensed material" (`docs/02-REQUIREMENTS/05-compliance-aup.md`
//! §2). The attestation is a stored, versioned artifact: one
//! immutable row per capture (`user_id`, `upload_id`,
//! `text_version`, `ip_hash`, `client`), and the job-create
//! gate ([`require_for_upload`]) rejects a job whose upload
//! has no attestation row for the caller with
//! [`AppError::AttestationMissing`] (PF-4001, 403).
//!
//! # Capture point
//!
//! [`capture`] is the upload-create call: Phase 2's intake
//! calls it with the attestation the client sent in the
//! upload-create body (`POST /v1/uploads` already names the
//! attestation required — the api-spec row), and the job-create
//! handler references the row at job creation. Until Phase 2
//! lands there is no HTTP capture endpoint: upload-create is
//! still the 501 stub, so the only production caller today is
//! the job-create gate, and the tests capture through this
//! service directly.
//!
//! # What the service does NOT do
//!
//! - It does not enforce the text version: capture stores
//!   whatever non-empty version the client attested to, and
//!   the gate accepts any stored row. A version bump that
//!   forces re-accept is P1-095's consent-versioning work
//!   (and P1-099's suite proves it) — this ticket records the
//!   version, it does not police it.
//! - It writes no audit row: the attestation row itself is
//!   the auditable record (immutable, timestamped, hashed IP
//!   — FR-C-01's "auditable and exportable"). The admin
//!   review queue is P1-098.
//! - It does not link the job: `jobs.attestation_id` is the
//!   Phase 2 intake's column to fill when it creates the job
//!   row (the FK exists since migration 004); the gate only
//!   proves the row exists first.

use crate::checksum::sha256_hex;
use crate::db::DbPool;
use crate::error::AppError;
use crate::repos::attestations::{self, Attestation};
use crate::repos::{uploads, RepoError};
use thiserror::Error;
use uuid::Uuid;

/// The current attestation text version — the version of the
/// text in `docs/02-REQUIREMENTS/05-compliance-aup.md` §2
/// that clients attest to today. Bumped by a ticket when the
/// text changes (P1-095 owns the bump mechanics); old rows
/// keep their version — rows are immutable, so history is
/// preserved.
pub const CURRENT_TEXT_VERSION: &str = "v1";

/// Which client captured the attestation — the `client`
/// column's typed form (`web` | `api_key`, the migration's
/// CHECK). The handler derives it from the credential that
/// authenticated the request (a JWT session is `web`, an API
/// key is `api_key`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Client {
    /// A browser session (JWT).
    Web,
    /// An API-key call.
    ApiKey,
}

impl Client {
    /// The stored string (`web` | `api_key`).
    pub fn as_str(&self) -> &'static str {
        match self {
            Client::Web => "web",
            Client::ApiKey => "api_key",
        }
    }
}

/// What attestation capture / the gate can fail with.
#[derive(Debug, Error)]
pub enum AttestationError {
    /// A repository call failed.
    #[error("attestation check failed")]
    Db(#[from] RepoError),
    /// A caller-supplied value failed validation before any
    /// SQL ran (an empty text version or an empty IP — the
    /// P1-087 audit-service precedent: `InvalidInput`, nothing
    /// written).
    #[error("invalid attestation input: {field}: {reason}")]
    InvalidInput {
        field: &'static str,
        reason: &'static str,
    },
    /// The upload does not exist, or belongs to another user
    /// — the P1-045 IDOR rule: a foreign upload is a 404, never
    /// a leak that it exists.
    #[error("upload not found")]
    UploadNotFound,
    /// No attestation row exists for this user and upload —
    /// the job-create gate's rejection (PF-4001).
    #[error("attestation missing")]
    AttestationMissing,
}

impl From<AttestationError> for AppError {
    fn from(err: AttestationError) -> Self {
        match err {
            AttestationError::Db(RepoError::NotFound) => AppError::NotFound,
            AttestationError::Db(_) => AppError::Internal,
            AttestationError::InvalidInput { .. } => AppError::ValidationFailed,
            AttestationError::UploadNotFound => AppError::NotFound,
            AttestationError::AttestationMissing => AppError::AttestationMissing,
        }
    }
}

/// The sha256 hex of a client IP (NFR-42 — only the hash is
/// ever stored, never the address). Deterministic: the same
/// IP always hashes the same way, so two captures from one
/// address are correlatable without ever storing it.
pub fn hash_ip(ip: &str) -> String {
    sha256_hex(ip.as_bytes())
}

/// Capture an attestation for an upload: the upload-create
/// call. Validates the inputs, proves the upload exists and
/// belongs to the caller (a foreign upload is a 404 — the
/// P1-045 IDOR rule), then inserts the immutable row. The
/// `text_version` is the version the client attested to
/// (non-empty; the current one is [`CURRENT_TEXT_VERSION`]).
/// The IP is hashed before it reaches the repository — no
/// function below this one ever sees a raw address.
pub async fn capture(
    pool: &DbPool,
    user_id: Uuid,
    upload_id: Uuid,
    text_version: &str,
    ip: &str,
    client: Client,
) -> Result<Attestation, AttestationError> {
    if text_version.is_empty() {
        return Err(AttestationError::InvalidInput {
            field: "text_version",
            reason: "must not be empty",
        });
    }
    if ip.is_empty() {
        return Err(AttestationError::InvalidInput {
            field: "ip",
            reason: "must not be empty",
        });
    }
    let upload = uploads::by_id(pool, upload_id).await?;
    match upload {
        Some(upload) if upload.user_id == user_id => {}
        _ => return Err(AttestationError::UploadNotFound),
    }
    let row = attestations::insert(
        pool,
        user_id,
        upload_id,
        text_version,
        &hash_ip(ip),
        client.as_str(),
    )
    .await?;
    Ok(row)
}

/// The job-create gate: the caller's newest attestation for
/// the upload, or [`AttestationError::AttestationMissing`]
/// when the user never attested for it. Scoped to the
/// caller's rows — another user's attestation for the same
/// upload does not satisfy it (and the quota pre-check
/// already 404s a foreign upload before this runs).
pub async fn require_for_upload(
    pool: &DbPool,
    user_id: Uuid,
    upload_id: Uuid,
) -> Result<Attestation, AttestationError> {
    match attestations::for_upload(pool, user_id, upload_id).await? {
        Some(row) => Ok(row),
        None => Err(AttestationError::AttestationMissing),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_stores_the_migration_strings() {
        assert_eq!(Client::Web.as_str(), "web");
        assert_eq!(Client::ApiKey.as_str(), "api_key");
    }

    #[test]
    fn current_text_version_is_v1() {
        assert_eq!(CURRENT_TEXT_VERSION, "v1");
        assert!(!CURRENT_TEXT_VERSION.is_empty());
    }

    #[test]
    fn hash_ip_is_a_deterministic_sha256_not_the_address() {
        let hash = hash_ip("203.0.113.7");
        assert_eq!(hash.len(), 64, "sha256 hex is 64 chars: {hash}");
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(hash, "203.0.113.7", "the raw IP is never the stored value");
        assert_eq!(hash, hash_ip("203.0.113.7"), "same IP hashes the same way");
        assert_ne!(
            hash,
            hash_ip("203.0.113.8"),
            "another IP hashes differently"
        );
    }

    #[test]
    fn error_mapping_is_the_problem_contract() {
        assert_eq!(
            AppError::from(AttestationError::AttestationMissing).code(),
            "PF-4001"
        );
        assert_eq!(
            AppError::from(AttestationError::AttestationMissing).status(),
            403
        );
        assert_eq!(
            AppError::from(AttestationError::UploadNotFound).code(),
            "PF-2010"
        );
        assert_eq!(
            AppError::from(AttestationError::InvalidInput {
                field: "text_version",
                reason: "must not be empty",
            })
            .code(),
            "PF-2002"
        );
        assert_eq!(
            AppError::from(AttestationError::Db(RepoError::Db(
                sqlx::Error::RowNotFound
            )))
            .code(),
            "PF-5000"
        );
    }
}
