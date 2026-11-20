//! Audit query API (P1-088, FR-C-03): `GET /v1/admin/audit`
//! — the read side of the audit trail the P1-087 service
//! writes. Admin + support (the read-only role) may call
//! it; a verified plain user or an API key gets 403
//! `PF-1002` from the extractor, no credential 401
//! `PF-1001`.
//!
//! The list is newest first, cursor-paginated over
//! `(created_at, id)` (the `detag_core::pagination` keyset
//! — the same contract as the users search and the jobs
//! list). The filters are exact matches, combined with
//! AND: `actor` is the acting user's id, `action` the
//! dotted action name, `target` the target's id (any target
//! type — the type rides on every row), `from`/`to` the
//! half-open created-at window `[from, to)` (a row exactly
//! at `to` belongs to the next window, never to this one;
//! an inverted window — `from` after `to` — is an empty
//! page, not an error: the SQL yields nothing, so there is
//! nothing to validate). A tampered cursor is 422
//! `PF-2002`; `limit` clamps to 1..=100 (default 20).
//!
//! No audit row: a read creates nothing durable (P1-046 —
//! reads are not audited). No user-existence check: the
//! actor filter names a historical actor, and a list
//! endpoint answers "what do they have?" — an unknown actor
//! id is an empty page, not a 404 (the P1-080 precedent: a
//! list never 404s on its filter).

use chrono::{DateTime, Utc};
use detag_core::pagination::{clamp_limit, Cursor, Page};
use detag_core::repos::audit_logs::{self, AuditFilters, AuditLog};
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::AppState;

/// One audit row as the query API returns it: the stored
/// fields, verbatim. `before`/`after` carry hashes, never
/// raw PII (NFR-42 — the writers' contract, see the audit
/// service); this endpoint does not redact, it only reads.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct AuditLogItem {
    /// The row's id.
    #[schema(value_type = String, example = "123e4567-e89b-42d3-a456-426614174109")]
    pub id: Uuid,
    /// The acting user, if any (anonymous or system events
    /// carry `None`).
    #[schema(value_type = Option<String>, example = "123e4567-e89b-42d3-a456-426614174109")]
    pub actor_user_id: Option<Uuid>,
    /// Which client acted (`web`, `api_key`, ...).
    pub actor_client: Option<String>,
    /// The dotted action name, e.g. `user.suspended`.
    pub action: String,
    /// What the action targeted, e.g. `user`.
    pub target_type: String,
    /// The target's id (a user UUID once it exists).
    pub target_id: Option<String>,
    /// State before the action, if meaningful.
    pub before: Option<Value>,
    /// State after the action (hashes, never raw PII).
    pub after: Option<Value>,
    /// Why, when the action needs a reason.
    pub reason: Option<String>,
    /// When the event landed.
    pub created_at: DateTime<Utc>,
}

impl From<AuditLog> for AuditLogItem {
    fn from(row: AuditLog) -> Self {
        Self {
            id: row.id,
            actor_user_id: row.actor_user_id,
            actor_client: row.actor_client,
            action: row.action,
            target_type: row.target_type,
            target_id: row.target_id,
            before: row.before,
            after: row.after,
            reason: row.reason,
            created_at: row.created_at,
        }
    }
}

/// One page of the audit trail: the api-spec pagination
/// shape (`{"items": [...], "next_cursor": "..."}` —
/// `next_cursor` omitted on the last page).
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct AuditPage {
    /// This page's rows, newest first (at most the
    /// requested limit).
    pub items: Vec<AuditLogItem>,
    /// The cursor for the next page, or `None` when this
    /// is the last page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// What the audit query can fail with. `InvalidCursor` is
/// the tampered cursor (422 `PF-2002` at the handler); the
/// rest are infrastructure failures — the handler answers
/// 500 `PF-5000`.
#[derive(Debug, Error)]
pub enum AdminAuditError {
    /// The cursor does not decode.
    #[error("invalid pagination cursor")]
    InvalidCursor,
    /// A database read failed.
    #[error("audit read failed")]
    Db(#[from] detag_core::repos::RepoError),
}

/// List one page of the audit trail: the filters, the
/// clamped limit, the decoded cursor. Fetches `limit + 1`
/// rows so the page knows whether a next page exists. The
/// filters ride in one `AuditFilters` (five fields — a
/// separate argument each would trip clippy's
/// `too-many-arguments`).
pub async fn list_audit(
    state: &AppState,
    filters: AuditFilters,
    limit: Option<u32>,
    cursor: Option<String>,
) -> Result<AuditPage, AdminAuditError> {
    let limit = clamp_limit(limit);
    let cursor = match cursor {
        Some(raw) => {
            let decoded = Cursor::decode(&raw).map_err(|_| AdminAuditError::InvalidCursor)?;
            // A decoded cursor can still carry a `ts` outside
            // chrono's range (`i64::MAX` decodes fine) — the
            // repo's `from_timestamp_millis(...).expect(...)`
            // would panic on it, so the range check lives here:
            // out of range is a tampered cursor (422), never a
            // 500 (the pagination contract: a tampered cursor is
            // a client error).
            DateTime::from_timestamp_millis(decoded.ts).ok_or(AdminAuditError::InvalidCursor)?;
            Some(decoded)
        }
        None => None,
    };
    let rows = audit_logs::list_filtered(&state.pool, &filters, cursor, limit + 1).await?;
    let page = Page::from_window(rows, limit, |row| Cursor {
        // `timestamp_millis()` truncates to milliseconds —
        // the same truncation the repo's keyset compares on,
        // so the cursor points at exactly this row.
        ts: row.created_at.timestamp_millis(),
        id: row.id,
    });
    Ok(AuditPage {
        items: page.items.into_iter().map(AuditLogItem::from).collect(),
        next_cursor: page.next_cursor,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn row(action: &str, created_at: DateTime<Utc>) -> AuditLog {
        AuditLog {
            id: Uuid::now_v7(),
            actor_user_id: None,
            actor_client: Some("test".to_string()),
            action: action.to_string(),
            target_type: "user".to_string(),
            target_id: Some("target-1".to_string()),
            before: None,
            after: None,
            reason: Some("test reason".to_string()),
            created_at,
        }
    }

    #[test]
    fn item_carries_the_stored_row_verbatim() {
        let at = Utc.with_ymd_and_hms(2026, 11, 19, 12, 0, 0).unwrap();
        let item = AuditLogItem::from(row("user.suspended", at));
        assert_eq!(item.action, "user.suspended");
        assert_eq!(item.target_type, "user");
        assert_eq!(item.target_id.as_deref(), Some("target-1"));
        assert_eq!(item.actor_client.as_deref(), Some("test"));
        assert_eq!(item.reason.as_deref(), Some("test reason"));
        assert_eq!(item.created_at, at);
    }

    #[test]
    fn page_serializes_to_the_spec_shape() {
        let at = Utc.with_ymd_and_hms(2026, 11, 19, 12, 0, 0).unwrap();
        let page = AuditPage {
            items: vec![AuditLogItem::from(row("user.suspended", at))],
            next_cursor: Some("opaque-cursor".to_string()),
        };
        let json = serde_json::to_value(&page).expect("serializes");
        assert_eq!(json["items"][0]["action"], "user.suspended");
        assert_eq!(json["next_cursor"], "opaque-cursor");
        let last = AuditPage {
            items: vec![],
            next_cursor: None,
        };
        let json = serde_json::to_value(&last).expect("serializes");
        assert!(
            json.get("next_cursor").is_none(),
            "no next_cursor on the last page: {json}"
        );
    }
}
