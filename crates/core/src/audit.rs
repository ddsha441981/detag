//! detag-core::audit — the audit service (P1-087, E1.10,
//! FR-C-03).
//!
//! The audit trail (`audit_logs`, migration 004) is append-only
//! and this service is its ONLY write path. Every privileged or
//! provenance-related action in the codebase records its event
//! here — actor, action, target, before/after, reason — and no
//! other code writes an audit row directly:
//!
//! - [`record`] is the single entry point. It validates the
//!   event's shape ([`validate`]) and then inserts through the
//!   repository. It takes `impl Executor`, so the same call
//!   works against the pool and inside a transaction (pass
//!   `&mut *conn` inside `tx::with_tx`, as the repo docs say).
//! - The repository's `insert` is `pub(crate)`, so no other
//!   crate can write a row at all — the compiler rejects it.
//! - Inside this crate, a unit test below scans the workspace
//!   source and fails if any file other than this one calls
//!   the repository insert, so a future direct writer is caught
//!   by `cargo test`, not by review.
//!
//! Append-only enforcement is the DATABASE trigger
//! (`audit_logs_append_only`, migration 004, reaffirmed by the
//! P1-007 purge migration): UPDATE/DELETE raise `table
//! audit_logs is append-only`. It is deliberately NOT a GRANT —
//! this repo's dev database has a single role (`pf`) that owns
//! the tables, and an owner bypasses ACLs, so a REVOKE would be
//! a no-op here; the trigger binds every role including the
//! owner. True role separation (a dedicated app role without
//! UPDATE/DELETE on this table) is a deployment concern for the
//! production database. The live test
//! `crates/core/tests/audit_live.rs` proves UPDATE/DELETE are
//! denied on a row the service wrote, against the current
//! schema.
//!
//! Validation (shape only — the service never inspects payload
//! contents): the action is a dotted name (`register.attempt`,
//! `billing.usage.reconciled`, ...), non-empty, with no
//! whitespace; the target type is non-empty, with no whitespace.
//! A malformed event fails with [`RepoError::InvalidInput`]
//! and writes nothing. Payload privacy (NFR-42) stays the
//! caller's job: `before`/`after` carry hashes, never raw PII
//! (see the repository docs) — the service cannot detect PII
//! in an arbitrary JSON value, so it does not pretend to.
//!
//! Reads are NOT the service's job: the P1-088 query API and
//! the tests read through the repository's `list_for_target` /
//! `list_for_action` directly.

use crate::repos::{audit_logs, RepoError};
use uuid::Uuid;

/// A stored audit row — re-exported so callers use one import
/// (`detag_core::audit::{record, NewAuditLog, AuditLog}`).
pub use crate::repos::audit_logs::AuditLog;

/// One audit event to record — re-exported, see [`AuditLog`].
pub use crate::repos::audit_logs::NewAuditLog;

/// Validate an event's shape before it is written. Pure — no
/// DB access, so the unit tests below cover it without a
/// database.
///
/// Rules: `action` is a non-empty dotted name with no
/// whitespace; `target_type` is non-empty with no whitespace.
/// `actor_*`, `target_id`, `before`, `after` and `reason` are
/// unconstrained (a `None` actor is an anonymous or system
/// event; a `None` target id is an attempt that has no user
/// yet).
pub fn validate(event: &NewAuditLog) -> Result<(), RepoError> {
    if event.action.is_empty()
        || !event.action.contains('.')
        || event.action.chars().any(char::is_whitespace)
    {
        return Err(RepoError::InvalidInput {
            field: "action",
            reason: "must be a non-empty dotted name without whitespace",
        });
    }
    if event.target_type.is_empty() || event.target_type.chars().any(char::is_whitespace) {
        return Err(RepoError::InvalidInput {
            field: "target_type",
            reason: "must be non-empty without whitespace",
        });
    }
    Ok(())
}

/// Record one audit event — the only way an audit row is
/// written. Validates first (a malformed event writes
/// nothing), then inserts and returns the row's id.
pub async fn record(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    event: &NewAuditLog,
) -> Result<Uuid, RepoError> {
    validate(event)?;
    audit_logs::insert(pool, event).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn event(action: &str, target_type: &str) -> NewAuditLog {
        NewAuditLog {
            actor_user_id: None,
            actor_client: Some("test".to_string()),
            action: action.to_string(),
            target_type: target_type.to_string(),
            target_id: Some("target-1".to_string()),
            before: Some(json!({ "status": "active" })),
            after: Some(json!({ "status": "suspended" })),
            reason: Some("test reason".to_string()),
        }
    }

    #[test]
    fn validate_accepts_a_well_formed_event() {
        assert!(validate(&event("user.suspended", "user")).is_ok());
        assert!(validate(&event("billing.subscription.created", "subscription")).is_ok());
        // Anonymous/system events (no actor, no target id) are
        // still well-formed — the shape rules do not require them.
        let mut anon = event("register.attempt", "user");
        anon.actor_user_id = None;
        anon.target_id = None;
        assert!(validate(&anon).is_ok());
    }

    #[test]
    fn validate_rejects_an_empty_action() {
        assert!(matches!(
            validate(&event("", "user")),
            Err(RepoError::InvalidInput { field, .. }) if field == "action"
        ));
    }

    #[test]
    fn validate_rejects_an_undotted_action() {
        assert!(matches!(
            validate(&event("suspended", "user")),
            Err(RepoError::InvalidInput { field, .. }) if field == "action"
        ));
    }

    #[test]
    fn validate_rejects_whitespace_in_action_or_target_type() {
        assert!(matches!(
            validate(&event("user. suspended", "user")),
            Err(RepoError::InvalidInput { field, .. }) if field == "action"
        ));
        assert!(matches!(
            validate(&event("user.suspended", "user ")),
            Err(RepoError::InvalidInput { field, .. }) if field == "target_type"
        ));
    }

    #[test]
    fn validate_rejects_an_empty_target_type() {
        assert!(matches!(
            validate(&event("user.suspended", "")),
            Err(RepoError::InvalidInput { field, .. }) if field == "target_type"
        ));
    }

    /// No direct repository writes: the only file in the
    /// workspace that may call `audit_logs::insert` is this
    /// service module itself (via [`record`]). Every other
    /// writer — in this crate or any other — must go through
    /// the service, which validates first. The repository's
    /// `insert` is `pub(crate)`, so other crates cannot call it
    /// at all; this test catches a direct call inside this
    /// crate, where the compiler would allow it.
    ///
    /// The scan reads the workspace source tree (skipping
    /// `target/`, `.git/` and `.scratch/`), so it runs wherever
    /// the checkout lives.
    #[test]
    fn no_direct_repo_writes_outside_this_service() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..");
        let allowed = std::fs::canonicalize(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("src")
                .join("audit.rs"),
        )
        .expect("canonicalize audit.rs");
        let mut offenders = Vec::new();
        let mut stack = vec![root.join("crates"), root.join("apps")];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if path.is_dir() {
                    if !matches!(name.as_ref(), "target" | ".git" | ".scratch") {
                        stack.push(path);
                    }
                    continue;
                }
                if path.extension().is_some_and(|ext| ext == "rs") {
                    let Ok(text) = std::fs::read_to_string(&path) else {
                        continue;
                    };
                    // The walk builds unnormalized paths (the root
                    // carries `../..` segments), so compare
                    // canonical paths.
                    let canon = std::fs::canonicalize(&path).unwrap_or(path.clone());
                    if text.contains("audit_logs::insert") && canon != allowed {
                        offenders.push(path);
                    }
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "direct audit_logs::insert calls outside crates/core/src/audit.rs — \
             write through detag_core::audit::record instead: {offenders:?}"
        );
    }
}
