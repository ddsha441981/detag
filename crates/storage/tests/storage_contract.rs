//! The `StorageProvider` contract, run against every implementation
//! that can run without a server (P0-053).
//!
//! Today that is only [`MemoryStorage`]: the S3 implementation's
//! presign methods are covered by their own offline tests in
//! `s3.rs` (presigning is local HMAC signing), and its data-plane
//! methods need a real backend — MinIO in compose (P0-054) and CI.
//! When a live-backend harness exists, point this same contract at
//! it: the tests below must pass unchanged against any conforming
//! implementation.

use std::time::Duration;

use detag_storage::memory::MemoryStorage;
use detag_storage::provider::{CompletedPart, StorageError, StorageProvider};

const KEY: &str = "dev/2026-11/job-contract/input.png";
const OTHER_KEY: &str = "dev/2026-11/job-contract/result.png";

#[tokio::test]
async fn contract_multipart_lifecycle() {
    let store = MemoryStorage::new();
    let upload_id = store.init_multipart(KEY).await.expect("init");
    assert!(!upload_id.is_empty());

    let part_url = store
        .presign_put_part(KEY, &upload_id, 2, Duration::from_secs(900))
        .await
        .expect("presign part 2");
    assert!(part_url.url.as_str().contains("partNumber=2"));
    assert!(part_url.url.as_str().contains(&upload_id));

    // Completing with an unknown upload id fails (client error).
    assert!(matches!(
        store
            .complete_multipart(
                KEY,
                "no-such-upload",
                vec![CompletedPart {
                    part_number: 1,
                    etag: "\"e1\"".to_string(),
                }]
            )
            .await,
        Err(StorageError::Invalid(_))
    ));

    let done = store
        .complete_multipart(
            KEY,
            &upload_id,
            vec![
                CompletedPart {
                    part_number: 1,
                    etag: "\"e1\"".to_string(),
                },
                CompletedPart {
                    part_number: 2,
                    etag: "\"e2\"".to_string(),
                },
            ],
        )
        .await
        .expect("complete");
    assert_eq!(done.key, KEY);
    assert!(done.etag.unwrap().ends_with("-2"), "etag ends -partcount");

    // A completed upload id cannot be completed twice.
    assert!(matches!(
        store
            .complete_multipart(
                KEY,
                &upload_id,
                vec![CompletedPart {
                    part_number: 1,
                    etag: "\"e1\"".to_string(),
                }]
            )
            .await,
        Err(StorageError::Invalid(_))
    ));

    // head finds it; get presigns it.
    let head = store.head(KEY).await.expect("head after complete");
    assert_eq!(head.key, KEY);
    let get_url = store
        .presign_get(KEY, Duration::from_secs(3600))
        .await
        .expect("presign get");
    assert_eq!(get_url.expires_in, Duration::from_secs(3600));
    assert!(get_url.url.as_str().contains(KEY));

    // Abort is idempotent, even for an unknown upload.
    store
        .abort_multipart(KEY, "no-such-upload")
        .await
        .expect("abort unknown is Ok");

    // Delete removes it; deleting again is Ok; head then 404s.
    store.delete(KEY).await.expect("delete");
    store.delete(KEY).await.expect("delete again is Ok");
    assert!(matches!(
        store.head(KEY).await,
        Err(StorageError::NotFound { key }) if key == KEY
    ));
}

#[tokio::test]
async fn contract_list_by_prefix() {
    let store = MemoryStorage::new();
    store.seed(KEY, "\"e1\"").await;
    store.seed(OTHER_KEY, "\"e2\"").await;
    store
        .seed("dev/2026-11/other-job/input.png", "\"e3\"")
        .await;
    store
        .seed("prod/2026-11/job-contract/input.png", "\"e4\"")
        .await;

    let all = store
        .list_by_prefix("dev/2026-11/job-contract/")
        .await
        .expect("list prefix");
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].key, KEY);
    assert_eq!(all[1].key, OTHER_KEY);

    let one = store.list_by_prefix(KEY).await.expect("list exact");
    assert_eq!(one.len(), 1);

    let none = store
        .list_by_prefix("dev/2026-11/no-such/")
        .await
        .expect("list empty");
    assert!(none.is_empty());
}

#[tokio::test]
async fn contract_rejects_bad_keys_everywhere() {
    let store = MemoryStorage::new();
    let bad = "../escape";
    assert!(matches!(
        store.init_multipart(bad).await,
        Err(StorageError::Invalid(_))
    ));
    assert!(matches!(
        store.head(bad).await,
        Err(StorageError::Invalid(_))
    ));
    assert!(matches!(
        store.delete(bad).await,
        Err(StorageError::Invalid(_))
    ));
    assert!(matches!(
        store.list_by_prefix(bad).await,
        Err(StorageError::Invalid(_))
    ));
    assert!(matches!(
        store.presign_get(bad, Duration::from_secs(60)).await,
        Err(StorageError::Invalid(_))
    ));
    assert!(matches!(
        store
            .presign_put_part(bad, "u", 1, Duration::from_secs(60))
            .await,
        Err(StorageError::Invalid(_))
    ));
}

#[tokio::test]
async fn contract_abort_then_complete_fails() {
    let store = MemoryStorage::new();
    let upload_id = store.init_multipart(KEY).await.expect("init");
    store.abort_multipart(KEY, &upload_id).await.expect("abort");
    assert!(matches!(
        store
            .complete_multipart(
                KEY,
                &upload_id,
                vec![CompletedPart {
                    part_number: 1,
                    etag: "\"e1\"".to_string(),
                }]
            )
            .await,
        Err(StorageError::Invalid(_))
    ));
}
