//! Deletion planner (P0-060, E0.6).
//!
//! Pure function over the bucket listing — no I/O, no database —
//! so every branch is unit-testable. The rules, in order:
//!
//! 1. **Expired artifact** (`expires_at` passed, FR-J-09): delete.
//!    Expired wins over "known": an expired key is in both sets.
//! 2. **Known key** (an artifact row or an upload input row
//!    references it): keep. Inputs have no `expires_at`, so the GC
//!    never deletes them; the ILM 3-day ceiling
//!    (`ops/minio/lifecycle.json`) is the input backstop.
//! 3. **Orphan** (no database row references the key): delete only
//!    when its `last_modified` is older than the grace period
//!    (default 24 h — an upload in flight always has its row
//!    first, but the grace is the belt to that brace). An orphan
//!    with no `last_modified` is kept: without a timestamp it
//!    cannot prove it is old, and the GC fails safe.

use std::collections::HashSet;

use chrono::{DateTime, Duration, Utc};
use detag_storage::provider::ObjectSummary;

/// What the GC does with one object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GcAction {
    /// Leave the object alone.
    Keep,
    /// An artifact whose retention expired (FR-J-09).
    DeleteExpired,
    /// No database row references this key, and it is older than
    /// the orphan grace period.
    DeleteOrphan,
}

/// Classify every listed object. Returns `(key, action)` pairs in
/// the listing's order (the provider returns keys sorted).
pub fn plan(
    objects: &[ObjectSummary],
    expired: &HashSet<String>,
    known: &HashSet<String>,
    now: DateTime<Utc>,
    orphan_grace: Duration,
) -> Vec<(String, GcAction)> {
    objects
        .iter()
        .map(|obj| {
            let action = if expired.contains(&obj.key) {
                GcAction::DeleteExpired
            } else if known.contains(&obj.key) {
                GcAction::Keep
            } else {
                match obj.last_modified {
                    Some(modified) if now - modified > orphan_grace => GcAction::DeleteOrphan,
                    _ => GcAction::Keep,
                }
            };
            (obj.key.clone(), action)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(key: &str, modified: Option<DateTime<Utc>>) -> ObjectSummary {
        ObjectSummary {
            key: key.to_string(),
            size: 10,
            last_modified: modified,
        }
    }

    fn sets(expired: &[&str], known: &[&str]) -> (HashSet<String>, HashSet<String>) {
        (
            expired.iter().map(|s| s.to_string()).collect(),
            known.iter().map(|s| s.to_string()).collect(),
        )
    }

    #[test]
    fn expired_wins_over_known() {
        let now = Utc::now();
        let (expired, known) = sets(&["a/output.png"], &["a/output.png", "a/input.png"]);
        let objects = vec![
            summary("a/output.png", Some(now)),
            summary("a/input.png", Some(now)),
        ];
        let planned = plan(&objects, &expired, &known, now, Duration::hours(24));
        assert_eq!(planned[0].1, GcAction::DeleteExpired);
        assert_eq!(planned[1].1, GcAction::Keep);
    }

    #[test]
    fn old_orphan_deleted_young_orphan_kept() {
        let now = Utc::now();
        let (expired, known) = sets(&[], &[]);
        let objects = vec![
            summary("old.png", Some(now - Duration::hours(25))),
            summary("young.png", Some(now - Duration::hours(1))),
        ];
        let planned = plan(&objects, &expired, &known, now, Duration::hours(24));
        assert_eq!(planned[0].1, GcAction::DeleteOrphan);
        assert_eq!(planned[1].1, GcAction::Keep);
    }

    #[test]
    fn orphan_without_timestamp_is_kept() {
        let now = Utc::now();
        let (expired, known) = sets(&[], &[]);
        let objects = vec![summary("mystery.png", None)];
        let planned = plan(&objects, &expired, &known, now, Duration::hours(24));
        assert_eq!(planned[0].1, GcAction::Keep);
    }

    #[test]
    fn orphan_exactly_at_grace_is_kept() {
        // `>` not `>=`: an object exactly grace-old has not yet
        // outlived the grace period.
        let now = Utc::now();
        let (expired, known) = sets(&[], &[]);
        let objects = vec![summary("edge.png", Some(now - Duration::hours(24)))];
        let planned = plan(&objects, &expired, &known, now, Duration::hours(24));
        assert_eq!(planned[0].1, GcAction::Keep);
    }
}
