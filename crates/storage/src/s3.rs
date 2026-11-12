//! S3-backed [`StorageProvider`] (P0-053): `aws-sdk-s3` against
//! MinIO in dev and S3/R2 in prod.
//!
//! One client, path-style URLs (`{endpoint}/{bucket}/{key}`), so
//! the same code works against MinIO (which needs path style) and
//! real S3. Credentials are static, from [`S3Config`] — which the
//! caller builds from `PF_S3_*` settings (`detag-core` owns the
//! settings; this crate takes a plain struct so it never depends
//! on core).
//!
//! Presigning is purely local (HMAC signing, no network), so the
//! presign methods are unit-testable with dummy credentials and
//! no server. The other methods need a real backend; their
//! contract is covered by the shared tests in
//! `tests/storage_contract.rs`, run against
//! [`crate::memory::MemoryStorage`].

use std::time::Duration;

use aws_sdk_s3::config::{BehaviorVersion, Credentials, Region};
use aws_sdk_s3::presigning::PresigningConfig;
use aws_sdk_s3::types::{CompletedMultipartUpload, CompletedPart as SdkCompletedPart};
use aws_sdk_s3::Client;
use aws_smithy_types::error::metadata::ProvideErrorMetadata;
use aws_smithy_types::DateTime as SdkDateTime;
use chrono::{DateTime, Utc};

use crate::provider::{
    validate_expiry, validate_key, validate_part_number, validate_upload_id, CompletedPart,
    CompletedUpload, ObjectHead, ObjectSummary, PresignedUrl, StorageError, StorageProvider,
};

/// Static configuration for [`S3Storage`]. Built by the caller
/// from `PF_S3_*` (endpoint, bucket, region, access key, secret
/// key). No defaults here: `detag-core::Settings` owns the
/// dev defaults (`localhost:9000`, `detag-dev`, `minioadmin`,
/// `us-east-1`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct S3Config {
    /// S3-compatible endpoint, e.g. `http://localhost:9000` (MinIO
    /// dev) or `https://s3.us-east-1.amazonaws.com`.
    pub endpoint: String,
    /// Bucket name, e.g. `detag-dev`. Must exist; bucket creation
    /// is an ops concern (P0-054).
    pub bucket: String,
    /// Region, e.g. `us-east-1`. MinIO ignores it; S3 needs it for
    /// signing.
    pub region: String,
    /// Access key id.
    pub access_key: String,
    /// Secret access key.
    pub secret_key: String,
}

impl S3Config {
    /// Reject an unusable config before any client is built.
    pub fn validate(&self) -> Result<(), StorageError> {
        for (name, value) in [
            ("endpoint", &self.endpoint),
            ("bucket", &self.bucket),
            ("region", &self.region),
            ("access_key", &self.access_key),
            ("secret_key", &self.secret_key),
        ] {
            if value.trim().is_empty() {
                return Err(StorageError::InvalidConfig(format!(
                    "{name} must not be empty"
                )));
            }
        }
        url::Url::parse(&self.endpoint)
            .map_err(|e| StorageError::InvalidConfig(format!("endpoint is not a URL: {e}")))?;
        Ok(())
    }
}

/// S3-compatible storage. Cheap to clone (the client is
/// internally reference-counted); share one instance per process.
#[derive(Debug, Clone)]
pub struct S3Storage {
    client: Client,
    bucket: String,
}

impl S3Storage {
    /// Build a client from `config`. Validates the config first;
    /// no network call is made here (the client is lazy).
    pub fn new(config: S3Config) -> Result<Self, StorageError> {
        config.validate()?;
        let credentials = Credentials::new(
            &config.access_key,
            &config.secret_key,
            None,
            None,
            "detag-static",
        );
        let sdk_config = aws_sdk_s3::config::Builder::new()
            .behavior_version(BehaviorVersion::latest())
            .region(Region::new(config.region.clone()))
            .credentials_provider(credentials)
            .endpoint_url(&config.endpoint)
            .force_path_style(true)
            .build();
        Ok(Self {
            client: Client::from_conf(sdk_config),
            bucket: config.bucket,
        })
    }

    /// The bucket this instance talks to.
    pub fn bucket(&self) -> &str {
        &self.bucket
    }

    /// Presign a request builder. `build` is the fluent builder's
    /// `presigned` call; kept generic over the two presignable
    /// builders via a closure so the two methods stay identical.
    async fn presign<F, Fut, E>(
        &self,
        expires_in: Duration,
        build: F,
    ) -> Result<PresignedUrl, StorageError>
    where
        F: FnOnce(PresigningConfig) -> Fut,
        Fut: std::future::Future<
            Output = Result<
                aws_sdk_s3::presigning::PresignedRequest,
                aws_sdk_s3::error::SdkError<E>,
            >,
        >,
        E: ProvideErrorMetadata + std::error::Error + Send + Sync + 'static,
    {
        let config = PresigningConfig::expires_in(expires_in)
            .map_err(|e| StorageError::Invalid(format!("bad presign expiry: {e}")))?;
        let request = build(config).await.map_err(map_sdk_error)?;
        let url = url::Url::parse(request.uri())
            .map_err(|_| StorageError::backend_msg("presigned URL did not parse"))?;
        Ok(PresignedUrl { url, expires_in })
    }
}

/// Map any SDK error to [`StorageError::Backend`], keeping the
/// original as the source for logs.
fn map_sdk_error<E>(err: aws_sdk_s3::error::SdkError<E>) -> StorageError
where
    E: std::error::Error + Send + Sync + 'static,
{
    StorageError::Backend(Box::new(err))
}

/// The service-side error code of an SDK error (`NoSuchKey`,
/// `NoSuchUpload`, ...), if the backend answered at all.
fn service_code<E: ProvideErrorMetadata>(err: &aws_sdk_s3::error::SdkError<E>) -> Option<&str> {
    match err {
        aws_sdk_s3::error::SdkError::ServiceError(ctx) => ctx.err().code(),
        _ => None,
    }
}

/// Convert an SDK timestamp to chrono (lossless: seconds +
/// sub-second nanos).
fn to_chrono(ts: SdkDateTime) -> DateTime<Utc> {
    DateTime::from_timestamp(ts.secs(), ts.subsec_nanos()).unwrap_or(DateTime::<Utc>::MIN_UTC)
}

impl StorageProvider for S3Storage {
    async fn presign_put_part(
        &self,
        key: &str,
        upload_id: &str,
        part_number: i32,
        expires_in: Duration,
    ) -> Result<PresignedUrl, StorageError> {
        validate_key(key)?;
        validate_upload_id(upload_id)?;
        validate_part_number(part_number)?;
        validate_expiry(expires_in)?;
        let bucket = self.bucket.clone();
        let key = key.to_string();
        let upload_id = upload_id.to_string();
        self.presign(expires_in, |config| {
            self.client
                .upload_part()
                .bucket(bucket)
                .key(key)
                .upload_id(upload_id)
                .part_number(part_number)
                .presigned(config)
        })
        .await
    }

    async fn presign_get(
        &self,
        key: &str,
        expires_in: Duration,
    ) -> Result<PresignedUrl, StorageError> {
        validate_key(key)?;
        validate_expiry(expires_in)?;
        let bucket = self.bucket.clone();
        let key = key.to_string();
        self.presign(expires_in, |config| {
            self.client
                .get_object()
                .bucket(bucket)
                .key(key)
                .presigned(config)
        })
        .await
    }

    async fn init_multipart(&self, key: &str) -> Result<String, StorageError> {
        validate_key(key)?;
        let out = self
            .client
            .create_multipart_upload()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(map_sdk_error)?;
        out.upload_id().map(ToString::to_string).ok_or_else(|| {
            StorageError::backend_msg("create_multipart_upload returned no upload id")
        })
    }

    async fn complete_multipart(
        &self,
        key: &str,
        upload_id: &str,
        parts: Vec<CompletedPart>,
    ) -> Result<CompletedUpload, StorageError> {
        validate_key(key)?;
        validate_upload_id(upload_id)?;
        if parts.is_empty() {
            return Err(StorageError::Invalid(
                "complete_multipart needs at least one part".to_string(),
            ));
        }
        for part in &parts {
            validate_part_number(part.part_number)?;
            if part.etag.trim().is_empty() {
                return Err(StorageError::Invalid(
                    "part etag must not be empty".to_string(),
                ));
            }
        }
        let sdk_parts: Vec<SdkCompletedPart> = parts
            .into_iter()
            .map(|p| {
                SdkCompletedPart::builder()
                    .part_number(p.part_number)
                    .e_tag(p.etag)
                    .build()
            })
            .collect();
        let upload = CompletedMultipartUpload::builder()
            .set_parts(Some(sdk_parts))
            .build();
        let out = self
            .client
            .complete_multipart_upload()
            .bucket(&self.bucket)
            .key(key)
            .upload_id(upload_id)
            .multipart_upload(upload)
            .send()
            .await
            .map_err(|e| match service_code(&e) {
                Some("NoSuchUpload") => {
                    StorageError::Invalid("unknown multipart upload id".to_string())
                }
                _ => map_sdk_error(e),
            })?;
        Ok(CompletedUpload {
            key: key.to_string(),
            etag: out.e_tag().map(ToString::to_string),
        })
    }

    async fn abort_multipart(&self, key: &str, upload_id: &str) -> Result<(), StorageError> {
        validate_key(key)?;
        validate_upload_id(upload_id)?;
        match self
            .client
            .abort_multipart_upload()
            .bucket(&self.bucket)
            .key(key)
            .upload_id(upload_id)
            .send()
            .await
        {
            Ok(_) => Ok(()),
            // Idempotent abort: an unknown upload is already gone.
            Err(e) if service_code(&e) == Some("NoSuchUpload") => Ok(()),
            Err(e) => Err(map_sdk_error(e)),
        }
    }

    async fn head(&self, key: &str) -> Result<ObjectHead, StorageError> {
        validate_key(key)?;
        let out = self
            .client
            .head_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(|e| match service_code(&e) {
                Some("NotFound" | "NoSuchKey" | "404") => StorageError::NotFound {
                    key: key.to_string(),
                },
                _ => map_sdk_error(e),
            })?;
        Ok(ObjectHead {
            key: key.to_string(),
            size: u64::try_from(out.content_length().unwrap_or(0)).unwrap_or(0),
            content_type: out.content_type().map(ToString::to_string),
            etag: out.e_tag().map(ToString::to_string),
            last_modified: out.last_modified().map(|ts| to_chrono(*ts)),
        })
    }

    async fn delete(&self, key: &str) -> Result<(), StorageError> {
        validate_key(key)?;
        // S3 delete is idempotent: a missing key is Ok.
        self.client
            .delete_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(map_sdk_error)?;
        Ok(())
    }

    async fn list_by_prefix(&self, prefix: &str) -> Result<Vec<ObjectSummary>, StorageError> {
        // A prefix is not a key, but the same traversal rules apply.
        if !prefix.is_empty() {
            validate_key(prefix)?;
        }
        let mut out = Vec::new();
        let mut continuation: Option<String> = None;
        loop {
            let mut req = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(prefix);
            if let Some(token) = continuation {
                req = req.continuation_token(token);
            }
            let page = req.send().await.map_err(map_sdk_error)?;
            for obj in page.contents() {
                let Some(key) = obj.key() else { continue };
                out.push(ObjectSummary {
                    key: key.to_string(),
                    size: u64::try_from(obj.size().unwrap_or(0)).unwrap_or(0),
                    last_modified: obj.last_modified().map(|ts| to_chrono(*ts)),
                });
            }
            if page.is_truncated() != Some(true) {
                break;
            }
            continuation = page.next_continuation_token().map(ToString::to_string);
            if continuation.is_none() {
                // Truncated but no token: the backend is lying or
                // the page is corrupt — stop rather than loop forever.
                break;
            }
        }
        out.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> S3Config {
        S3Config {
            endpoint: "http://localhost:9000".to_string(),
            bucket: "detag-dev".to_string(),
            region: "us-east-1".to_string(),
            access_key: "minioadmin".to_string(),
            secret_key: "minioadmin".to_string(),
        }
    }

    fn storage() -> S3Storage {
        S3Storage::new(test_config()).expect("test config builds")
    }

    #[test]
    fn config_validation_rejects_empty_fields() {
        for (field, mut cfg) in [
            ("endpoint", test_config()),
            ("bucket", test_config()),
            ("region", test_config()),
            ("access_key", test_config()),
            ("secret_key", test_config()),
        ] {
            match field {
                "endpoint" => cfg.endpoint.clear(),
                "bucket" => cfg.bucket.clear(),
                "region" => cfg.region.clear(),
                "access_key" => cfg.access_key.clear(),
                "secret_key" => cfg.secret_key.clear(),
                _ => unreachable!(),
            }
            assert!(
                matches!(cfg.validate(), Err(StorageError::InvalidConfig(_))),
                "{field} empty must fail validation"
            );
        }
        let mut bad_url = test_config();
        bad_url.endpoint = "not a url".to_string();
        assert!(matches!(
            bad_url.validate(),
            Err(StorageError::InvalidConfig(_))
        ));
    }

    #[tokio::test]
    async fn presign_get_shape() {
        // No server needed: presigning is local HMAC signing.
        let store = storage();
        let key = "dev/2026-11/job-1/result.png";
        let presigned = store
            .presign_get(key, Duration::from_secs(3600))
            .await
            .expect("presign_get works offline");
        assert_eq!(presigned.expires_in, Duration::from_secs(3600));
        let url = presigned.url.as_str();
        // Path-style: endpoint + bucket + key.
        assert!(
            url.starts_with("http://localhost:9000/detag-dev/dev/2026-11/job-1/result.png?"),
            "unexpected presigned URL: {url}"
        );
        assert!(url.contains("X-Amz-Algorithm=AWS4-HMAC-SHA256"), "{url}");
        assert!(url.contains("X-Amz-Signature="), "{url}");
        assert!(url.contains("X-Amz-Expires=3600"), "{url}");
    }

    #[tokio::test]
    async fn presign_put_part_shape() {
        let store = storage();
        let presigned = store
            .presign_put_part(
                "dev/2026-11/job-1/input.png",
                "upload-id-123",
                1,
                Duration::from_secs(900),
            )
            .await
            .expect("presign_put_part works offline");
        let url = presigned.url.as_str();
        assert!(
            url.starts_with("http://localhost:9000/detag-dev/dev/2026-11/job-1/input.png?"),
            "unexpected presigned URL: {url}"
        );
        assert!(url.contains("partNumber=1"), "{url}");
        assert!(url.contains("uploadId=upload-id-123"), "{url}");
        assert!(url.contains("X-Amz-Expires=900"), "{url}");
    }

    #[tokio::test]
    async fn presign_rejects_bad_args_without_touching_the_network() {
        let store = storage();
        // Bad key / part number / expiry fail before any signing.
        assert!(matches!(
            store
                .presign_get("../escape", Duration::from_secs(60))
                .await,
            Err(StorageError::Invalid(_))
        ));
        assert!(matches!(
            store
                .presign_put_part("k", "u", 0, Duration::from_secs(60))
                .await,
            Err(StorageError::Invalid(_))
        ));
        assert!(matches!(
            store
                .presign_get("k", Duration::from_secs(8 * 24 * 3600))
                .await,
            Err(StorageError::Invalid(_))
        ));
        assert!(matches!(
            store
                .presign_put_part("k", "", 1, Duration::from_secs(60))
                .await,
            Err(StorageError::Invalid(_))
        ));
    }

    #[tokio::test]
    async fn complete_rejects_empty_parts_locally() {
        let store = storage();
        assert!(matches!(
            store.complete_multipart("k", "u", vec![]).await,
            Err(StorageError::Invalid(_))
        ));
    }
}
