//! GC run (P0-060, E0.6).
//!
//! Lists the whole bucket, plans deletions with [`plan::plan`],
//! and applies them — or, in dry-run mode (the default), counts
//! what *would* be deleted without deleting anything. The whole-
//! bucket list is the one exception to the provider trait's
//! "prefixes are narrow" note: the GC's job is the whole bucket,
//! and the provider pages internally, so the list is bounded by
//! bucket size (small by design — ILM caps object lifetime at
//! 72 h). A failed delete never aborts the run: it is recorded
//! in the report and the exit code goes non-zero, so the next run
//! retries it.

use std::collections::HashSet;

use chrono::{DateTime, Duration, Utc};
use detag_storage::provider::{StorageError, StorageProvider};

use crate::plan::{plan, GcAction};

/// Run options.
#[derive(Debug, Clone, Copy)]
pub struct GcOpts {
    /// Count only; delete nothing.
    pub dry_run: bool,
    /// Orphans younger than this are kept (default 24 h).
    pub orphan_grace: Duration,
}

impl Default for GcOpts {
    fn default() -> Self {
        Self {
            dry_run: true,
            orphan_grace: Duration::hours(24),
        }
    }
}

/// What one run did.
#[derive(Debug, Default)]
pub struct GcReport {
    /// Objects listed.
    pub scanned: usize,
    /// Expired artifacts deleted (or counted, in dry-run).
    pub deleted_expired: usize,
    /// Orphans deleted (or counted, in dry-run).
    pub deleted_orphans: usize,
    /// Objects left alone.
    pub kept: usize,
    /// `(key, error)` for deletes that failed. Keys are PII-free
    /// by layout (ADR-0011); the error Display is PII-free too.
    pub failures: Vec<(String, String)>,
}

/// List, plan, apply. `expired` and `known` come from the
/// artifacts repo (`expired_storage_keys` / `all_storage_keys`).
pub async fn run_gc(
    provider: &impl StorageProvider,
    expired: &HashSet<String>,
    known: &HashSet<String>,
    opts: &GcOpts,
) -> Result<GcReport, StorageError> {
    let objects = provider.list_by_prefix("").await?;
    let now: DateTime<Utc> = Utc::now();
    let mut report = GcReport {
        scanned: objects.len(),
        ..Default::default()
    };
    for (key, action) in plan(&objects, expired, known, now, opts.orphan_grace) {
        match action {
            GcAction::Keep => report.kept += 1,
            GcAction::DeleteExpired | GcAction::DeleteOrphan => {
                if opts.dry_run {
                    match action {
                        GcAction::DeleteExpired => report.deleted_expired += 1,
                        _ => report.deleted_orphans += 1,
                    }
                } else {
                    match provider.delete(&key).await {
                        Ok(()) => match action {
                            GcAction::DeleteExpired => report.deleted_expired += 1,
                            _ => report.deleted_orphans += 1,
                        },
                        Err(err) => report.failures.push((key, err.to_string())),
                    }
                }
            }
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use detag_storage::memory::MemoryStorage;
    use detag_storage::provider::CompletedPart;

    async fn seed(store: &MemoryStorage, key: &str) {
        let upload_id = store.init_multipart(key).await.expect("init");
        store
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
    }

    fn sets(expired: &[&str], known: &[&str]) -> (HashSet<String>, HashSet<String>) {
        (
            expired.iter().map(|s| s.to_string()).collect(),
            known.iter().map(|s| s.to_string()).collect(),
        )
    }

    #[tokio::test]
    async fn dry_run_deletes_nothing() {
        let store = MemoryStorage::new();
        seed(&store, "dev/2026-11/job-1/output.png").await;
        seed(&store, "dev/2026-11/job-1/input.png").await;
        let (expired, known) = sets(
            &["dev/2026-11/job-1/output.png"],
            &[
                "dev/2026-11/job-1/output.png",
                "dev/2026-11/job-1/input.png",
            ],
        );
        let report = run_gc(&store, &expired, &known, &GcOpts::default())
            .await
            .expect("run_gc");
        assert_eq!(report.scanned, 2);
        assert_eq!(report.deleted_expired, 1);
        assert_eq!(report.kept, 1);
        assert!(report.failures.is_empty());
        // Nothing was actually deleted.
        assert_eq!(store.list_by_prefix("").await.expect("list").len(), 2);
    }

    #[tokio::test]
    async fn execute_deletes_only_the_expired_key() {
        let store = MemoryStorage::new();
        seed(&store, "dev/2026-11/job-1/output.png").await;
        seed(&store, "dev/2026-11/job-1/input.png").await;
        // A young orphan (completed just now, inside the grace):
        // listed, kept, never deleted.
        seed(&store, "dev/2026-11/job-1/stray.png").await;
        let (expired, known) = sets(
            &["dev/2026-11/job-1/output.png"],
            &[
                "dev/2026-11/job-1/output.png",
                "dev/2026-11/job-1/input.png",
            ],
        );
        let opts = GcOpts {
            dry_run: false,
            ..Default::default()
        };
        let report = run_gc(&store, &expired, &known, &opts)
            .await
            .expect("run_gc");
        assert_eq!(report.scanned, 3);
        assert_eq!(report.deleted_expired, 1);
        assert_eq!(report.deleted_orphans, 0);
        assert_eq!(report.kept, 2);
        assert!(report.failures.is_empty());
        let remaining = store.list_by_prefix("").await.expect("list");
        let keys: Vec<&str> = remaining.iter().map(|o| o.key.as_str()).collect();
        assert_eq!(
            keys,
            vec!["dev/2026-11/job-1/input.png", "dev/2026-11/job-1/stray.png",]
        );
    }
}
