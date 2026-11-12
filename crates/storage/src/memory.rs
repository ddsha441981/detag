//! In-memory [`StorageProvider`] for tests (P0-053).
//!
//! Models the **control plane** only: `init` / `complete` /
//! `abort` / `head` / `delete` / `list` behave like the real
//! backend, but no bytes are ever stored — the client uploads
//! part bytes straight to the presigned URL, so this fake never
//! sees them. `complete_multipart` therefore records an object
//! whose size is 0 and whose ETag is derived from the part ETags.
//! Presigned URLs use the `memory://detag.test/` scheme: they are
//! not fetchable, they only let flow tests assert on the URL
//! shape (bucket, key, part number, upload id, expiry).
//!
//! Use it in API/worker unit tests that need a provider without a
//! server. Never in production: it holds everything in process
//! memory and loses it on drop.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::time::Duration;

use chrono::{DateTime, Utc};
use tokio::sync::Mutex;

use crate::provider::{
    validate_expiry, validate_key, validate_part_number, validate_upload_id, CompletedPart,
    CompletedUpload, ObjectHead, ObjectSummary, PresignedUrl, StorageError, StorageProvider,
};

#[derive(Debug, Clone)]
struct MemoryObject {
    etag: String,
    content_type: Option<String>,
    last_modified: DateTime<Utc>,
}

/// In-memory storage. Construct with [`MemoryStorage::new`]; all
/// state lives behind async mutexes, so one instance is safe to
/// share across tasks.
#[derive(Debug, Default)]
pub struct MemoryStorage {
    objects: Mutex<HashMap<String, MemoryObject>>,
    uploads: Mutex<HashMap<String, String>>,
}

impl MemoryStorage {
    /// An empty fake.
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed one object (tests that need a `head`/`list` fixture
    /// without going through multipart).
    pub async fn seed(&self, key: &str, etag: &str) {
        validate_key(key).expect("seed key must be valid");
        self.objects.lock().await.insert(
            key.to_string(),
            MemoryObject {
                etag: etag.to_string(),
                content_type: None,
                last_modified: Utc::now(),
            },
        );
    }

    fn fake_url(&self, key: &str, query: &str, expires_in: Duration) -> PresignedUrl {
        let query = if query.is_empty() {
            format!("X-Amz-Expires={}", expires_in.as_secs())
        } else {
            format!("{query}&X-Amz-Expires={}", expires_in.as_secs())
        };
        let url = url::Url::parse(&format!("memory://detag.test/{key}?{query}"))
            .expect("fake presigned URL parses");
        PresignedUrl { url, expires_in }
    }
}

impl StorageProvider for MemoryStorage {
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
        // The upload id must exist (it was issued by init_multipart).
        if !self.uploads.lock().await.contains_key(upload_id) {
            return Err(StorageError::Invalid(
                "unknown multipart upload id".to_string(),
            ));
        }
        Ok(self.fake_url(
            key,
            &format!("partNumber={part_number}&uploadId={upload_id}"),
            expires_in,
        ))
    }

    async fn presign_get(
        &self,
        key: &str,
        expires_in: Duration,
    ) -> Result<PresignedUrl, StorageError> {
        validate_key(key)?;
        validate_expiry(expires_in)?;
        Ok(self.fake_url(key, "", expires_in))
    }

    async fn init_multipart(&self, key: &str) -> Result<String, StorageError> {
        validate_key(key)?;
        let upload_id = uuid::Uuid::now_v7().to_string();
        self.uploads
            .lock()
            .await
            .insert(upload_id.clone(), key.to_string());
        Ok(upload_id)
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
        let mut uploads = self.uploads.lock().await;
        match uploads.remove(upload_id) {
            Some(stored_key) if stored_key == key => {}
            _ => {
                return Err(StorageError::Invalid(
                    "unknown multipart upload id".to_string(),
                ));
            }
        }
        drop(uploads);
        // ETag derived from the part ETags (same shape as S3's
        // `hash-partcount`, minus real bytes).
        let mut hasher = DefaultHasher::new();
        for part in &parts {
            part.etag.hash(&mut hasher);
        }
        let etag = format!("{:x}-{}", hasher.finish(), parts.len());
        self.objects.lock().await.insert(
            key.to_string(),
            MemoryObject {
                etag: etag.clone(),
                content_type: None,
                last_modified: Utc::now(),
            },
        );
        Ok(CompletedUpload {
            key: key.to_string(),
            etag: Some(etag),
        })
    }

    async fn abort_multipart(&self, key: &str, upload_id: &str) -> Result<(), StorageError> {
        validate_key(key)?;
        validate_upload_id(upload_id)?;
        // Idempotent: removing an unknown upload is Ok.
        self.uploads.lock().await.remove(upload_id);
        Ok(())
    }

    async fn head(&self, key: &str) -> Result<ObjectHead, StorageError> {
        validate_key(key)?;
        match self.objects.lock().await.get(key) {
            Some(obj) => Ok(ObjectHead {
                key: key.to_string(),
                size: 0,
                content_type: obj.content_type.clone(),
                etag: Some(obj.etag.clone()),
                last_modified: Some(obj.last_modified),
            }),
            None => Err(StorageError::NotFound {
                key: key.to_string(),
            }),
        }
    }

    async fn delete(&self, key: &str) -> Result<(), StorageError> {
        validate_key(key)?;
        self.objects.lock().await.remove(key);
        Ok(())
    }

    async fn list_by_prefix(&self, prefix: &str) -> Result<Vec<ObjectSummary>, StorageError> {
        if !prefix.is_empty() {
            validate_key(prefix)?;
        }
        let objects = self.objects.lock().await;
        let mut out: Vec<ObjectSummary> = objects
            .iter()
            .filter(|(key, _)| key.starts_with(prefix))
            .map(|(key, obj)| ObjectSummary {
                key: key.clone(),
                size: 0,
                last_modified: Some(obj.last_modified),
            })
            .collect();
        out.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn memory_fake_round_trip() {
        let store = MemoryStorage::new();
        let key = "dev/2026-11/job-1/input.png";
        let upload_id = store.init_multipart(key).await.expect("init");
        let url = store
            .presign_put_part(key, &upload_id, 1, Duration::from_secs(900))
            .await
            .expect("presign part");
        assert!(url.url.as_str().contains("partNumber=1"));
        let done = store
            .complete_multipart(
                key,
                &upload_id,
                vec![CompletedPart {
                    part_number: 1,
                    etag: "\"abc123\"".to_string(),
                }],
            )
            .await
            .expect("complete");
        assert_eq!(done.key, key);
        assert!(done.etag.is_some());
        let head = store.head(key).await.expect("head finds it");
        assert_eq!(head.etag, done.etag);
        assert!(matches!(
            store.head("dev/2026-11/job-1/missing.png").await,
            Err(StorageError::NotFound { .. })
        ));
    }
}
