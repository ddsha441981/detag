//! `StorageProvider`: the one trait every caller uses to talk to
//! object storage (P0-053, E0.6).
//!
//! The API and the worker never touch storage credentials and never
//! sit in the byte path: clients upload parts directly to presigned
//! URLs, and downloads go through presigned GETs. This trait is the
//! control plane — presign, multipart lifecycle, head, delete, list.
//!
//! Implementations:
//! - [`crate::s3::S3Storage`] — the real one, `aws-sdk-s3` against
//!   MinIO in dev and S3/R2 in prod (path-style URLs, so the same
//!   code works against MinIO).
//! - [`crate::memory::MemoryStorage`] — in-memory fake for tests
//!   and for API/worker unit tests that need a provider without a
//!   server. It models the control plane only: parts are never
//!   stored (the client uploads them straight to the presigned URL),
//!   so `complete_multipart` records an object whose size is 0 and
//!   whose ETag is derived from the part ETags.
//!
//! Key rules (enforced by [`validate_key`], called by every method
//! of every implementation — P0-056 adds the ADR-0011 layout
//! helpers on top):
//! - keys are `pf:`-prefixed layout keys in production; the trait
//!   itself only rejects what can never be a valid key: empty,
//!   leading `/`, a `..` segment, a backslash, whitespace or a
//!   control character. A `:` inside a key is fine (it is how
//!   multi-part keys are built).
//! - part numbers are 1..=10_000 (the S3 limit); presigned URLs
//!   expire in at most 7 days (the SigV4 limit).

use std::time::Duration;

use chrono::{DateTime, Utc};
use url::Url;

/// Presigned URLs live at most 7 days (SigV4 limit).
pub const MAX_PRESIGN_SECS: u64 = 7 * 24 * 3600;

/// S3 allows part numbers 1..=10_000.
pub const MAX_PART_NUMBER: i32 = 10_000;

/// A presigned URL: the URL itself plus how long it stays valid.
/// Callers compute the absolute deadline (`Utc::now() +
/// expires_in`) themselves; the trait never bakes wall-clock time
/// into the value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresignedUrl {
    /// The signed URL. For the S3 implementation this is a
    /// path-style URL (`{endpoint}/{bucket}/{key}` + SigV4 query).
    pub url: Url,
    /// How long the URL stays valid from issuance.
    pub expires_in: Duration,
}

/// One completed multipart part, as the client reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletedPart {
    /// 1-based part number.
    pub part_number: i32,
    /// The ETag the storage backend returned when the part was
    /// uploaded.
    pub etag: String,
}

/// What `complete_multipart` returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletedUpload {
    /// The object key that was completed.
    pub key: String,
    /// The final ETag (`hash-partcount` for multipart objects).
    pub etag: Option<String>,
}

/// Metadata about one object, from [`StorageProvider::head`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectHead {
    /// The object key.
    pub key: String,
    /// Size in bytes.
    pub size: u64,
    /// Stored content type, if the backend knows it.
    pub content_type: Option<String>,
    /// The object's ETag, if the backend knows it.
    pub etag: Option<String>,
    /// Last modification time, if the backend knows it.
    pub last_modified: Option<DateTime<Utc>>,
}

/// One row of a [`StorageProvider::list_by_prefix`] result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectSummary {
    /// The object key.
    pub key: String,
    /// Size in bytes.
    pub size: u64,
    /// Last modification time, if the backend knows it.
    pub last_modified: Option<DateTime<Utc>>,
}

/// Storage failures. The `Display` is deliberately short and
/// PII-free: keys in this system are PII-free by layout (ADR-0011),
/// and the backend's own message is never echoed — it stays in the
/// error source for logs. Callers that need an API error map this
/// to `AppError` at the boundary (first caller: P0-055).
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    /// The object does not exist. The key is included: keys are
    /// PII-free by layout, and the caller needs it to say *what*
    /// is missing.
    #[error("object not found: {key}")]
    NotFound {
        /// The missing key.
        key: String,
    },
    /// A caller argument is invalid: bad key, bad part number,
    /// empty upload id, expiry out of range.
    #[error("invalid storage argument: {0}")]
    Invalid(String),
    /// The provider was constructed with bad configuration
    /// (empty endpoint/bucket, unparsable endpoint URL).
    #[error("invalid storage configuration: {0}")]
    InvalidConfig(String),
    /// The backend failed (network, auth, 5xx, unexpected
    /// response). The source is kept for logs; the display never
    /// includes it.
    #[error("storage backend error")]
    Backend(#[source] Box<dyn std::error::Error + Send + Sync>),
}

impl StorageError {
    /// Build a `Backend` error from a message (for cases where
    /// there is no underlying error to box, e.g. a missing field
    /// in an otherwise-successful response).
    pub fn backend_msg(msg: &'static str) -> Self {
        Self::Backend(Box::new(std::io::Error::other(msg)))
    }
}

/// Reject what can never be a valid Detag storage key. Every
/// implementation calls this first, so a bad key fails the same
/// way against S3 and against the fake.
pub fn validate_key(key: &str) -> Result<(), StorageError> {
    if key.is_empty() {
        return Err(StorageError::Invalid("key must not be empty".to_string()));
    }
    if key.starts_with('/') {
        return Err(StorageError::Invalid(
            "key must not start with '/'".to_string(),
        ));
    }
    if key.contains('\\') {
        return Err(StorageError::Invalid(
            "key must not contain '\\'".to_string(),
        ));
    }
    if key.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(StorageError::Invalid(
            "key must not contain whitespace or control characters".to_string(),
        ));
    }
    if key.split('/').any(|seg| seg == "..") {
        return Err(StorageError::Invalid(
            "key must not contain a '..' segment".to_string(),
        ));
    }
    Ok(())
}

/// Validate a multipart part number (S3: 1..=10_000).
pub fn validate_part_number(part_number: i32) -> Result<(), StorageError> {
    if !(1..=MAX_PART_NUMBER).contains(&part_number) {
        return Err(StorageError::Invalid(format!(
            "part_number must be 1..={MAX_PART_NUMBER}, got {part_number}"
        )));
    }
    Ok(())
}

/// Validate a presign expiry (SigV4: at most 7 days, and positive).
pub fn validate_expiry(expires_in: Duration) -> Result<(), StorageError> {
    if expires_in.is_zero() {
        return Err(StorageError::Invalid(
            "expires_in must be positive".to_string(),
        ));
    }
    if expires_in.as_secs() > MAX_PRESIGN_SECS {
        return Err(StorageError::Invalid(format!(
            "expires_in must be at most 7 days, got {}s",
            expires_in.as_secs()
        )));
    }
    Ok(())
}

/// Validate a multipart upload id (opaque, issued by
/// `init_multipart`; must be non-empty).
pub fn validate_upload_id(upload_id: &str) -> Result<(), StorageError> {
    if upload_id.is_empty() {
        return Err(StorageError::Invalid(
            "upload_id must not be empty".to_string(),
        ));
    }
    Ok(())
}

/// The object-storage control plane (P0-053).
///
/// Methods take `&self` and are safe to call concurrently; the
/// S3 implementation holds a cheaply-cloneable client, the fake
/// guards its maps with a mutex. Native `async fn` in the trait,
/// matching the house style (`FlagStore` in `detag-core`): no
/// `async_trait` macro, so the trait is not object-safe — callers
/// hold the concrete type or a generic, never `dyn StorageProvider`.
pub trait StorageProvider: Send + Sync {
    /// Presigned PUT URL for one multipart part. The client uploads
    /// the part bytes straight to this URL; the api never sees them.
    fn presign_put_part(
        &self,
        key: &str,
        upload_id: &str,
        part_number: i32,
        expires_in: Duration,
    ) -> impl std::future::Future<Output = Result<PresignedUrl, StorageError>> + Send;

    /// Presigned GET URL for one object (downloads, per-plan TTL —
    /// the caller passes the TTL; FR-J-09).
    fn presign_get(
        &self,
        key: &str,
        expires_in: Duration,
    ) -> impl std::future::Future<Output = Result<PresignedUrl, StorageError>> + Send;

    /// Start a multipart upload; returns the backend's upload id.
    fn init_multipart(
        &self,
        key: &str,
    ) -> impl std::future::Future<Output = Result<String, StorageError>> + Send;

    /// Finish a multipart upload. `parts` must list every part in
    /// ascending order with the ETag the backend returned per part;
    /// the backend verifies part count and sizes (P0-057 owns the
    /// uploads-table state machine around this call).
    fn complete_multipart(
        &self,
        key: &str,
        upload_id: &str,
        parts: Vec<CompletedPart>,
    ) -> impl std::future::Future<Output = Result<CompletedUpload, StorageError>> + Send;

    /// Abort a multipart upload. Idempotent: aborting an unknown
    /// or already-finished upload is Ok.
    fn abort_multipart(
        &self,
        key: &str,
        upload_id: &str,
    ) -> impl std::future::Future<Output = Result<(), StorageError>> + Send;

    /// Metadata for one object; `NotFound` when the key is absent.
    fn head(
        &self,
        key: &str,
    ) -> impl std::future::Future<Output = Result<ObjectHead, StorageError>> + Send;

    /// Delete one object. Idempotent: deleting a missing key is Ok.
    fn delete(
        &self,
        key: &str,
    ) -> impl std::future::Future<Output = Result<(), StorageError>> + Send;

    /// Every object whose key starts with `prefix`, sorted by key.
    /// Prefixes here are narrow by construction (one job's
    /// directory); the implementation pages through the backend
    /// internally, so callers get the full list.
    fn list_by_prefix(
        &self,
        prefix: &str,
    ) -> impl std::future::Future<Output = Result<Vec<ObjectSummary>, StorageError>> + Send;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_key_accepts_layout_keys() {
        for key in [
            "dev/2026-11/019aa1b2-c3d4-7e5f-8000-000000000001/input.png",
            "pf:cache:plan:abc",
            "a/b/c.txt",
            "single",
        ] {
            assert!(validate_key(key).is_ok(), "{key}");
        }
    }

    #[test]
    fn validate_key_rejects_bad_keys() {
        for key in [
            "",
            "/abs/path",
            "a/../b",
            "a\\b",
            "a b",
            "a\tb",
            "a/../",
            "..",
        ] {
            assert!(validate_key(key).is_err(), "{key:?}");
        }
    }

    #[test]
    fn validate_part_number_bounds() {
        assert!(validate_part_number(1).is_ok());
        assert!(validate_part_number(10_000).is_ok());
        assert!(validate_part_number(0).is_err());
        assert!(validate_part_number(10_001).is_err());
        assert!(validate_part_number(-5).is_err());
    }

    #[test]
    fn validate_expiry_bounds() {
        assert!(validate_expiry(Duration::from_secs(60)).is_ok());
        assert!(validate_expiry(Duration::from_secs(7 * 24 * 3600)).is_ok());
        assert!(validate_expiry(Duration::ZERO).is_err());
        assert!(validate_expiry(Duration::from_secs(7 * 24 * 3600 + 1)).is_err());
    }

    #[test]
    fn validate_upload_id_rejects_empty() {
        assert!(validate_upload_id("abc").is_ok());
        assert!(validate_upload_id("").is_err());
    }

    #[test]
    fn error_display_is_short_and_pii_free() {
        let e = StorageError::NotFound {
            key: "dev/2026-11/job/input.png".to_string(),
        };
        assert_eq!(e.to_string(), "object not found: dev/2026-11/job/input.png");
        let backend = StorageError::backend_msg("boom");
        assert_eq!(backend.to_string(), "storage backend error");
        // The source is kept for logs even though Display hides it.
        assert!(std::error::Error::source(&backend).is_some());
    }
}
