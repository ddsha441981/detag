//! Storage integration suite (P0-061, E0.6).
//!
//! The same cases run against every `StorageProvider` that can
//! hold bytes: the offline mock ([`MemoryStorage`], always — this
//! is the "suite green offline" AC) and a real MinIO started by
//! testcontainers (the live test below, env-gated). The cases
//! assert only behaviour both backends share by contract: where
//! the backends genuinely differ, the suite says so instead of
//! asserting one backend's behaviour.
//!
//! One genuine difference shapes the suite: real S3 verifies a
//! part's ETag against bytes actually uploaded through it, so a
//! multipart complete with fabricated ETags fails on MinIO
//! (`InvalidPart`) while the fake derives its ETag from whatever
//! it is given. The cases therefore never fabricate ETags: they
//! get them through a [`PartSource`] — the fake one returns a
//! fabricated ETag (the fake's contract), the live one uploads
//! real bytes through the SDK client and returns the real ETag.
//! Non-final parts must be >= 5 MiB on S3, so the live source
//! uploads 5 MiB of zeros for part 1 and a few bytes after that.
//!
//! Live test: `PF_TEST_LIVE_MINIO=1 cargo test -p detag-storage
//! --test storage_integration live_minio`. Without the variable
//! the live test prints SKIP and passes — on this VM docker is
//! gone (DOCKER-001), so the live run waits for CI, where the
//! runner has docker. The live path is compile-checked here
//! (`cargo check --tests`); its first real run is in CI.
//!
//! Cases (spec): multipart happy path, part mismatch, presign
//! expiry, delete, key traversal — plus list scoping, which the
//! GC (P0-060) and the key scheme (P0-056) both rely on.

use std::time::Duration;

use detag_storage::memory::MemoryStorage;
use detag_storage::provider::{CompletedPart, CompletedUpload, StorageError, StorageProvider};

const BASE: &str = "dev/2026-11/job-it";

/// How a case obtains a part's ETag for `complete_multipart`.
// NOTE: native `async fn` in the trait, matching the house style
// (`FlagStore`, `StorageProvider`); the suite is generic over the
// source, never `dyn`, so object-safety is not an issue.
trait PartSource {
    async fn part_etag(
        &self,
        provider: &impl StorageProvider,
        key: &str,
        upload_id: &str,
        part_number: i32,
    ) -> String;
}

/// The fake's contract: any well-formed ETag completes.
struct FakeParts;

impl PartSource for FakeParts {
    async fn part_etag(
        &self,
        _provider: &impl StorageProvider,
        _key: &str,
        _upload_id: &str,
        part_number: i32,
    ) -> String {
        format!("\"etag-{part_number}\"")
    }
}

/// The live contract: upload real bytes through the SDK client
/// and return the ETag S3 assigned. Part 1 is 5 MiB (S3's minimum
/// for a non-final part); later parts are a few bytes.
struct SdkParts {
    client: aws_sdk_s3::Client,
    bucket: String,
}

impl PartSource for SdkParts {
    async fn part_etag(
        &self,
        _provider: &impl StorageProvider,
        key: &str,
        upload_id: &str,
        part_number: i32,
    ) -> String {
        let bytes: Vec<u8> = if part_number == 1 {
            vec![0u8; 5 * 1024 * 1024]
        } else {
            format!("part-{part_number}").into_bytes()
        };
        self.client
            .upload_part()
            .bucket(&self.bucket)
            .key(key)
            .upload_id(upload_id)
            .part_number(part_number)
            .body(aws_sdk_s3::primitives::ByteStream::from(bytes))
            .send()
            .await
            .expect("upload part")
            .e_tag()
            .expect("upload returns an ETag")
            .to_string()
    }
}

async fn complete_parts(
    provider: &impl StorageProvider,
    source: &impl PartSource,
    key: &str,
    upload_id: &str,
    count: i32,
) -> Result<CompletedUpload, StorageError> {
    let mut parts = Vec::new();
    for n in 1..=count {
        parts.push(CompletedPart {
            part_number: n,
            etag: source.part_etag(provider, key, upload_id, n).await,
        });
    }
    provider.complete_multipart(key, upload_id, parts).await
}

async fn complete_one(provider: &impl StorageProvider, source: &impl PartSource, key: &str) {
    let upload_id = provider.init_multipart(key).await.expect("init");
    complete_parts(provider, source, key, &upload_id, 1)
        .await
        .expect("complete");
}

/// The shared suite. Every case uses its own key prefix, so cases
/// cannot see each other's objects.
async fn run_suite(provider: &impl StorageProvider, source: &impl PartSource) {
    key_traversal_rejected(provider).await;
    multipart_happy_path(provider, source).await;
    part_mismatch_rejected(provider, source).await;
    presign_expiry(provider).await;
    delete_is_idempotent(provider, source).await;
    list_is_scoped_and_sorted(provider, source).await;
}

async fn key_traversal_rejected(provider: &impl StorageProvider) {
    // Runs first, on an empty store: nothing in this case may
    // create an object.
    for bad in [
        "../evil.png",
        "/abs/path.png",
        "a/../../b.png",
        "dev/2026-11/job-it/evil\0.png",
        "dev/2026-11/job-it/has space.png",
        "",
    ] {
        assert!(
            matches!(
                provider.init_multipart(bad).await,
                Err(StorageError::Invalid(_))
            ),
            "init must reject {bad:?}"
        );
        assert!(
            matches!(
                provider.presign_get(bad, Duration::from_secs(60)).await,
                Err(StorageError::Invalid(_))
            ),
            "presign_get must reject {bad:?}"
        );
        assert!(
            matches!(provider.head(bad).await, Err(StorageError::Invalid(_))),
            "head must reject {bad:?}"
        );
        assert!(
            matches!(provider.delete(bad).await, Err(StorageError::Invalid(_))),
            "delete must reject {bad:?}"
        );
    }
    assert!(
        provider.list_by_prefix("").await.expect("list").is_empty(),
        "traversal attempts must not create objects"
    );
}

async fn multipart_happy_path(provider: &impl StorageProvider, source: &impl PartSource) {
    let key = format!("{BASE}-happy/input.png");
    let upload_id = provider.init_multipart(&key).await.expect("init");
    assert!(!upload_id.is_empty());

    let part_url = provider
        .presign_put_part(&key, &upload_id, 1, Duration::from_secs(900))
        .await
        .expect("presign part 1");
    assert!(part_url.url.as_str().contains("partNumber=1"));
    assert!(part_url.url.as_str().contains(&upload_id));

    let done = complete_parts(provider, source, &key, &upload_id, 2)
        .await
        .expect("complete");
    assert_eq!(done.key, key);
    assert!(done.etag.is_some(), "completed object has an ETag");

    let head = provider.head(&key).await.expect("head finds it");
    assert_eq!(head.key, key);

    let listed = provider
        .list_by_prefix(&format!("{BASE}-happy/"))
        .await
        .expect("list");
    assert_eq!(
        listed.iter().map(|o| o.key.as_str()).collect::<Vec<_>>(),
        vec![key.as_str()]
    );

    provider.delete(&key).await.expect("delete");
    assert!(
        matches!(
            provider.head(&key).await,
            Err(StorageError::NotFound { .. })
        ),
        "deleted object must be gone"
    );
}

async fn part_mismatch_rejected(provider: &impl StorageProvider, source: &impl PartSource) {
    let key = format!("{BASE}-mismatch/input.png");
    let upload_id = provider.init_multipart(&key).await.expect("init");

    // Unknown upload id.
    assert!(matches!(
        provider
            .complete_multipart(
                &key,
                "no-such-upload",
                vec![CompletedPart {
                    part_number: 1,
                    etag: "\"e1\"".to_string(),
                }]
            )
            .await,
        Err(StorageError::Invalid(_))
    ));
    // No parts at all.
    assert!(matches!(
        provider.complete_multipart(&key, &upload_id, vec![]).await,
        Err(StorageError::Invalid(_))
    ));
    // Empty ETag.
    assert!(matches!(
        provider
            .complete_multipart(
                &key,
                &upload_id,
                vec![CompletedPart {
                    part_number: 1,
                    etag: "   ".to_string(),
                }]
            )
            .await,
        Err(StorageError::Invalid(_))
    ));
    // A bad part number.
    assert!(matches!(
        provider
            .complete_multipart(
                &key,
                &upload_id,
                vec![CompletedPart {
                    part_number: 0,
                    etag: "\"e1\"".to_string(),
                }]
            )
            .await,
        Err(StorageError::Invalid(_))
    ));

    // The failed completes must not consume the upload or create
    // the object: the correct complete still works.
    assert!(
        matches!(
            provider.head(&key).await,
            Err(StorageError::NotFound { .. })
        ),
        "failed completes must not create the object"
    );
    complete_parts(provider, source, &key, &upload_id, 1)
        .await
        .expect("correct complete after mismatches");
    provider.delete(&key).await.expect("cleanup");
}

async fn presign_expiry(provider: &impl StorageProvider) {
    let key = format!("{BASE}-presign/input.png");
    let upload_id = provider.init_multipart(&key).await.expect("init");

    // The requested expiry is in the URL (both backends mint
    // `X-Amz-Expires=<secs>`; the S3 one is a real SigV4 URL,
    // the fake one is the same shape for offline tests).
    let part_url = provider
        .presign_put_part(&key, &upload_id, 3, Duration::from_secs(60))
        .await
        .expect("presign part");
    assert!(part_url.url.as_str().contains("X-Amz-Expires=60"));
    assert!(part_url.url.as_str().contains("partNumber=3"));
    assert_eq!(part_url.expires_in, Duration::from_secs(60));

    let get_url = provider
        .presign_get(&key, Duration::from_secs(3600))
        .await
        .expect("presign get");
    assert!(get_url.url.as_str().contains("X-Amz-Expires=3600"));

    // Zero and over-7-days expiries are rejected by the shared
    // validator (the SigV4 cap).
    assert!(matches!(
        provider
            .presign_put_part(&key, &upload_id, 1, Duration::ZERO)
            .await,
        Err(StorageError::Invalid(_))
    ));
    assert!(matches!(
        provider
            .presign_get(&key, Duration::from_secs(7 * 24 * 3600 + 1))
            .await,
        Err(StorageError::Invalid(_))
    ));

    provider
        .abort_multipart(&key, &upload_id)
        .await
        .expect("abort");
}

async fn delete_is_idempotent(provider: &impl StorageProvider, source: &impl PartSource) {
    let key = format!("{BASE}-delete/input.png");
    complete_one(provider, source, &key).await;

    provider.delete(&key).await.expect("first delete");
    assert!(
        matches!(
            provider.head(&key).await,
            Err(StorageError::NotFound { .. })
        ),
        "deleted object must be gone"
    );
    // Deleting a missing key is Ok (P0-053 contract).
    provider.delete(&key).await.expect("second delete is Ok");
    assert!(
        provider
            .list_by_prefix(&format!("{BASE}-delete/"))
            .await
            .expect("list")
            .is_empty(),
        "prefix must be empty after delete"
    );
}

async fn list_is_scoped_and_sorted(provider: &impl StorageProvider, source: &impl PartSource) {
    let a1 = format!("{BASE}-list/a/1.png");
    let a2 = format!("{BASE}-list/a/2.png");
    let b1 = format!("{BASE}-list/b/1.png");
    complete_one(provider, source, &a2).await;
    complete_one(provider, source, &b1).await;
    complete_one(provider, source, &a1).await;

    let scoped = provider
        .list_by_prefix(&format!("{BASE}-list/a/"))
        .await
        .expect("list");
    assert_eq!(
        scoped.iter().map(|o| o.key.as_str()).collect::<Vec<_>>(),
        vec![a1.as_str(), a2.as_str()],
        "list is scoped to the prefix and sorted by key"
    );

    for key in [&a1, &a2, &b1] {
        provider.delete(key).await.expect("cleanup");
    }
}

#[tokio::test]
async fn offline_memory_fake() {
    run_suite(&MemoryStorage::new(), &FakeParts).await;
}

/// Live MinIO via testcontainers. Env-gated: set
/// `PF_TEST_LIVE_MINIO=1` to run it (CI does); otherwise it
/// prints SKIP and passes — docker is absent on this VM
/// (DOCKER-001).
#[tokio::test]
async fn live_minio_testcontainers() {
    use testcontainers::core::IntoContainerPort;
    use testcontainers::{runners::AsyncRunner, GenericImage, ImageExt};

    if std::env::var("PF_TEST_LIVE_MINIO").as_deref() != Ok("1") {
        eprintln!("SKIP: live MinIO needs PF_TEST_LIVE_MINIO=1 (docker absent here, DOCKER-001)");
        return;
    }

    let container = GenericImage::new("quay.io/minio/minio", "latest")
        .with_exposed_port(9000.tcp())
        .with_wait_for(testcontainers::core::WaitFor::message_on_stdout(
            "API: http://",
        ))
        .with_env_var("MINIO_ROOT_USER", "minioadmin")
        .with_env_var("MINIO_ROOT_PASSWORD", "minioadmin")
        .with_cmd(["server", "/data"])
        .start()
        .await
        .expect("MinIO container starts");
    let port = container
        .get_host_port_ipv4(9000)
        .await
        .expect("MinIO host port");
    let endpoint = format!("http://127.0.0.1:{port}");
    let bucket = format!("detag-test-{}", uuid::Uuid::now_v7());

    // A raw SDK client for bucket setup (bucket creation is an ops
    // concern — the provider deliberately has no create_bucket).
    let creds =
        aws_sdk_s3::config::Credentials::new("minioadmin", "minioadmin", None, None, "detag-test");
    let sdk_config = aws_sdk_s3::config::Builder::new()
        .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
        .region(aws_sdk_s3::config::Region::new("us-east-1"))
        .credentials_provider(creds)
        .endpoint_url(&endpoint)
        .force_path_style(true)
        .build();
    let client = aws_sdk_s3::Client::from_conf(sdk_config);
    // MinIO can still be warming up after the log line; retry the
    // create instead of failing on the first refused connection.
    let mut created = false;
    for _ in 0..20 {
        match client.create_bucket().bucket(&bucket).send().await {
            Ok(_) => {
                created = true;
                break;
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(500)).await,
        }
    }
    assert!(created, "test bucket {bucket} must be creatable");

    let storage = detag_storage::s3::S3Storage::new(detag_storage::s3::S3Config {
        endpoint,
        bucket: bucket.clone(),
        region: "us-east-1".to_string(),
        access_key: "minioadmin".to_string(),
        secret_key: "minioadmin".to_string(),
    })
    .expect("S3Storage builds");
    run_suite(
        &storage,
        &SdkParts {
            client: client.clone(),
            bucket: bucket.clone(),
        },
    )
    .await;

    // Best-effort cleanup: the suite deletes its own objects, so
    // the bucket should be empty; a leftover must not fail the run.
    let _ = client.delete_bucket().bucket(&bucket).send().await;
    // Container drops here and is removed by testcontainers.
}
