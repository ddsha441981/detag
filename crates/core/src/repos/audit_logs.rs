//! Audit-log repository (P1-001, E1.1). `audit_logs` is the
//! append-only audit trail (migration 004): one row per event,
//! `action` is a dotted name (`register.attempt`,
//! `register.success`, ...). The table is append-only by trigger
//! (UPDATE/DELETE raise) — this module only inserts and reads.
//!
//! Writes go through the audit service ONLY
//! ([`crate::audit::record`], P1-087, E1.10): `insert` is
//! `pub(crate)` so no other crate can write an audit row
//! directly, and every in-crate writer calls the service too —
//! a unit test in `crate::audit` scans the workspace and fails
//! the build if any other file calls this function. Readers
//! (`list_for_target`, `list_for_action`) stay public: the
//! P1-088 query API and the tests read through them.
//!
//! Privacy (NFR-42): no PII in the log. `target_id` carries a
//! user id (UUID) once the user exists, never a raw email; the
//! `after` payload of an anonymous attempt carries the email's
//! sha256, never the email itself. Callers build the payloads;
//! this module does not see a raw email at all.

use super::error::RepoError;
use crate::pagination::Cursor;
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
///
/// Crate-internal: the audit service ([`crate::audit::record`])
/// is the only caller. Anything else that needs an audit row
/// goes through the service, which validates the event first.
pub(crate) async fn insert(
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

/// The P1-088 query filters: every field optional, an absent
/// field means "no filter". `target` is the target's id (the
/// `target_id` column — usually a user UUID once the user
/// exists); the target *type* is not a filter, it rides on
/// every returned row. `from` is inclusive, `to` exclusive —
/// the half-open window `[from, to)`, so a row exactly at
/// `to` belongs to the next window, never to this one.
#[derive(Debug, Clone, Default)]
pub struct AuditFilters {
    /// Only rows this user acted on.
    pub actor_user_id: Option<Uuid>,
    /// Only rows with this exact dotted action name.
    pub action: Option<String>,
    /// Only rows targeting this id (any target type).
    pub target_id: Option<String>,
    /// Only rows at or after this instant.
    pub from: Option<DateTime<Utc>>,
    /// Only rows before this instant.
    pub to: Option<DateTime<Utc>>,
}

/// Read back one page of the audit trail (newest first) for
/// the P1-088 query API: the filters above, keyset pagination
/// over `(created_at, id)`, `limit + 1` rows so the caller can
/// tell whether a next page exists (the `detag_core::pagination`
/// `Page::from_window` contract). A `None` cursor starts at
/// the newest row.
///
/// The keyset compares on `date_trunc('milliseconds',
/// created_at)` — the cursor carries Unix milliseconds, and
/// the truncation is the same one the service's
/// `timestamp_millis()` applies when it builds the next
/// cursor, so the cursor points at exactly the last row of
/// the previous page. Rows are ordered by that truncated
/// timestamp, ties broken by `id` — the same order the
/// cursor walks, so a page boundary never repeats or drops
/// a row (two rows created in the same millisecond still
/// order deterministically by `id`).
///
/// Indexes (data-model, migration 004): the actor filter +
/// ordering ride `audit_logs_actor_created_idx`
/// (`actor_user_id, created_at DESC`); the target filter
/// rides `audit_logs_target_idx` (`target_type, target_id`)
/// — the filtered set is then sorted. The action and
/// `from`/`to` filters have no dedicated index (the
/// data-model names none for them): they filter the rows
/// the ordering scans. No new index is added here — the
/// ticket's "indexes per data-model" is the two that exist.
pub async fn list_filtered(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    filters: &AuditFilters,
    cursor: Option<Cursor>,
    limit: u32,
) -> Result<Vec<AuditLog>, RepoError> {
    match cursor {
        Some(cursor) => {
            let cursor_ts = DateTime::from_timestamp_millis(cursor.ts)
                .expect("a cursor ts is a valid Unix-millis timestamp");
            sqlx::query_as!(
                AuditLog,
                r#"SELECT id, actor_user_id, actor_client, action, target_type,
                          target_id, "before", "after", reason, created_at
                   FROM audit_logs
                   WHERE ($1::uuid IS NULL OR actor_user_id = $1)
                     AND ($2::text IS NULL OR action = $2)
                     AND ($3::text IS NULL OR target_id = $3)
                     AND ($4::timestamptz IS NULL OR created_at >= $4)
                     AND ($5::timestamptz IS NULL OR created_at < $5)
                     AND (date_trunc('milliseconds', created_at), id) < ($6, $7)
                   ORDER BY date_trunc('milliseconds', created_at) DESC, id DESC
                   LIMIT $8"#,
                filters.actor_user_id,
                filters.action,
                filters.target_id,
                filters.from,
                filters.to,
                cursor_ts,
                cursor.id,
                limit as i64,
            )
            .fetch_all(pool)
            .await
            .map_err(RepoError::from_sqlx)
        }
        None => sqlx::query_as!(
            AuditLog,
            r#"SELECT id, actor_user_id, actor_client, action, target_type,
                      target_id, "before", "after", reason, created_at
               FROM audit_logs
               WHERE ($1::uuid IS NULL OR actor_user_id = $1)
                 AND ($2::text IS NULL OR action = $2)
                 AND ($3::text IS NULL OR target_id = $3)
                 AND ($4::timestamptz IS NULL OR created_at >= $4)
                 AND ($5::timestamptz IS NULL OR created_at < $5)
               ORDER BY date_trunc('milliseconds', created_at) DESC, id DESC
               LIMIT $6"#,
            filters.actor_user_id,
            filters.action,
            filters.target_id,
            filters.from,
            filters.to,
            limit as i64,
        )
        .fetch_all(pool)
        .await
        .map_err(RepoError::from_sqlx),
    }
}
