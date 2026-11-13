//! Audit-log repository (P1-001, E1.1). `audit_logs` is the
//! append-only audit trail (migration 004): one row per event,
//! `action` is a dotted name (`register.attempt`,
//! `register.success`, ...). The table is append-only by trigger
//! (UPDATE/DELETE raise) — this module only inserts and reads.
//!
//! Privacy (NFR-42): no PII in the log. `target_id` carries a
//! user id (UUID) once the user exists, never a raw email; the
//! `after` payload of an anonymous attempt carries the email's
//! sha256, never the email itself. Callers build the payloads;
//! this module does not see a raw email at all.

use super::error::RepoError;
use chrono::{DateTime, Utc};
use serde_json::Value;
use uuid::Uuid;

/// One audit event to record.
#[derive(Debug, Clone)]
pub struct NewAuditLog {
    /// The acting user, if any (anonymous requests pass `None`).
    pub actor_user_id: Option<Uuid>,
    /// Which client acted (`web`, `api_key`, ...); `None` when unknown.
    pub actor_client: Option<String>,
    /// Dotted action name, e.g. `register.attempt`.
    pub action: String,
    /// What the action targeted, e.g. `user`.
    pub target_type: String,
    /// The target's id (a user UUID once it exists; `None` for an
    /// anonymous attempt that has no user yet).
    pub target_id: Option<String>,
    /// State before the action, if meaningful.
    pub before: Option<Value>,
    /// State after the action (never PII — hashes, not values).
    pub after: Option<Value>,
    /// Why, when the action needs a reason (revocations, denials).
    pub reason: Option<String>,
}

/// A stored audit row (the fields tests and E1.10 readers need).
#[derive(Debug, Clone, PartialEq)]
pub struct AuditLog {
    pub id: Uuid,
    pub actor_user_id: Option<Uuid>,
    pub actor_client: Option<String>,
    pub action: String,
    pub target_type: String,
    pub target_id: Option<String>,
    pub before: Option<Value>,
    pub after: Option<Value>,
    pub reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Append an event. Returns the row's id.
pub async fn insert(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    log: &NewAuditLog,
) -> Result<Uuid, RepoError> {
    let row = sqlx::query!(
        r#"INSERT INTO audit_logs
               (actor_user_id, actor_client, action, target_type,
                target_id, "before", "after", reason)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
           RETURNING id"#,
        log.actor_user_id,
        log.actor_client,
        log.action,
        log.target_type,
        log.target_id,
        log.before,
        log.after,
        log.reason,
    )
    .fetch_one(pool)
    .await
    .map_err(RepoError::from_sqlx)?;
    Ok(row.id)
}

/// Read back the events for one target (oldest first), as stored.
/// The audit reader for E1.10 builds on this; the register test
/// uses it to prove both events landed.
pub async fn list_for_target(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    target_type: &str,
    target_id: &str,
) -> Result<Vec<AuditLog>, RepoError> {
    sqlx::query_as!(
        AuditLog,
        r#"SELECT id, actor_user_id, actor_client, action, target_type,
                  target_id, "before", "after", reason, created_at
           FROM audit_logs
           WHERE target_type = $1 AND target_id = $2
           ORDER BY created_at ASC, id ASC"#,
        target_type,
        target_id,
    )
    .fetch_all(pool)
    .await
    .map_err(RepoError::from_sqlx)
}

/// Read back the events with one action (oldest first), as stored.
/// The register test uses it for the anonymous `register.attempt`
/// rows, which have no target id to look up by.
pub async fn list_for_action(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    action: &str,
) -> Result<Vec<AuditLog>, RepoError> {
    sqlx::query_as!(
        AuditLog,
        r#"SELECT id, actor_user_id, actor_client, action, target_type,
                  target_id, "before", "after", reason, created_at
           FROM audit_logs
           WHERE action = $1
           ORDER BY created_at ASC, id ASC"#,
        action,
    )
    .fetch_all(pool)
    .await
    .map_err(RepoError::from_sqlx)
}
