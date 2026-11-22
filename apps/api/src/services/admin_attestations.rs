//! Attestation admin review (P1-098, FR-C-01 / FR-W-04):
//! `GET /v1/admin/attestations` — the read side of the
//! P1-094 capture: an admin (or support) lists the
//! attestations newest first and sees which ones look
//! anomalous, with a link to the abuse queue for the same
//! user.
//!
//! The queue is a read model over rows that already exist
//! (the P1-096 precedent): no new table, no new event, no
//! write on the read (P1-046). The only derived value is
//! the velocity flag: an attestation is flagged when its
//! user attested more than [`VELOCITY_THRESHOLD`] times in
//! the 24 h window ending at that row's `created_at` — the
//! count comes from the window function in
//! `repos::attestations::list_filtered`, so the flag is
//! computed over the user's full history, not just the
//! page. The threshold is the agent's choice (ADR-0061):
//! each attestation is an explicit per-upload act, so 11+
//! in a day from one account is worth a human look. The
//! flag is a review signal, never a block — nothing here
//! rejects, holds, or suspends; the queue only surfaces
//! rows for a human to open next to the abuse queue.
//!
//! `flag_reasons` is an array, not a single bool, so a
//! future rule (a stale text version, a repeated IP hash
//! across accounts) can add a reason without a shape
//! change. The abuse-queue link is a same-origin relative
//! reference (`/v1/admin/abuse-reports?user_id=...`) —
//! the P1-098 addition to that endpoint is the `user_id`
//! filter, so the link lands on the user's reports, not
//! the unfiltered queue.

use chrono::{DateTime, Utc};
use detag_core::{
    pagination::{clamp_limit, Cursor, Page},
    repos::attestations::{self, AttestationRow},
};
use serde::Serialize;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::AppState;

/// The velocity window: the 24 h ending at a row's
/// `created_at` (ADR-0061).
pub const VELOCITY_WINDOW_HOURS: i64 = 24;

/// The velocity threshold: a row is flagged when its
/// user's attestation count in the window exceeds this
/// (ADR-0061 — the agent's choice, a review signal, not
/// a block).
pub const VELOCITY_THRESHOLD: i64 = 10;

/// The flag reason for a velocity anomaly — the only
/// reason v1 knows.
pub const VELOCITY_REASON: &str = "velocity";

/// The flag reasons for a velocity count: `["velocity"]`
/// when the count exceeds [`VELOCITY_THRESHOLD`], empty
/// otherwise. The boundary is strict — exactly the
/// threshold is not an anomaly.
pub fn flag_reasons_for(velocity_count: i64) -> Vec<&'static str> {
    if velocity_count > VELOCITY_THRESHOLD {
        vec![VELOCITY_REASON]
    } else {
        vec![]
    }
}

/// The abuse-queue link for an attestation's user: the
/// queue filtered to that user's reports (the P1-098
/// `user_id` filter on `GET /v1/admin/abuse-reports`).
/// A relative reference — the console is same-origin.
pub fn abuse_queue_url(user_id: Uuid) -> String {
    format!("/v1/admin/abuse-reports?user_id={user_id}")
}

/// One queue row as the API returns it: the stored
/// fields, verbatim, plus the velocity flag. `ip_hash` is
/// the sha256 of the client IP (NFR-42 — the raw address
/// is never stored); it is correlatable evidence for an
/// admin/support read, never a public value.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct AttestationItem {
    /// The attestation's id.
    #[schema(value_type = String, example = "123e4567-e89b-42d3-a456-426614174109")]
    pub id: Uuid,
    /// The attesting user.
    #[schema(value_type = String, example = "123e4567-e89b-42d3-a456-426614174109")]
    pub user_id: Uuid,
    /// The attested upload.
    #[schema(value_type = String, example = "123e4567-e89b-42d3-a456-426614174109")]
    pub upload_id: Uuid,
    /// The attestation text version the client attested to.
    pub text_version: String,
    /// The sha256 of the client IP (NFR-42).
    pub ip_hash: String,
    /// `web` | `api_key` — the client that captured it.
    pub client: String,
    /// When the attestation was captured.
    pub created_at: DateTime<Utc>,
    /// The user's attestation count in the 24 h window
    /// ending at this row's `created_at` (inclusive).
    pub velocity_count: i64,
    /// Whether this row is flagged as anomalous.
    pub flagged: bool,
    /// Why the row is flagged (`["velocity"]` when
    /// flagged, `[]` otherwise).
    pub flag_reasons: Vec<&'static str>,
    /// The abuse queue for this user (relative reference).
    pub abuse_queue_url: String,
}

impl AttestationItem {
    /// Build the item from a listed row: the flag is the
    /// threshold comparison on the row's velocity count.
    pub fn from_row(row: AttestationRow) -> Self {
        let flag_reasons = flag_reasons_for(row.velocity_count);
        let flagged = !flag_reasons.is_empty();
        let abuse_queue_url = abuse_queue_url(row.user_id);
        Self {
            id: row.id,
            user_id: row.user_id,
            upload_id: row.upload_id,
            text_version: row.text_version,
            ip_hash: row.ip_hash,
            client: row.client,
            created_at: row.created_at,
            velocity_count: row.velocity_count,
            flagged,
            flag_reasons,
            abuse_queue_url,
        }
    }
}

/// One page of the queue: the api-spec pagination shape
/// (`{"items": [...], "next_cursor": "..."}` —
/// `next_cursor` omitted on the last page).
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct AttestationsPage {
    /// This page's attestations, newest first (at most the
    /// requested limit).
    pub items: Vec<AttestationItem>,
    /// The cursor for the next page, or `None` when this
    /// is the last page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// What the review queue can fail with. `InvalidFlagged`
/// is a bad `?flagged=` value; `InvalidCursor` a cursor
/// that does not decode (or its `ts` outside chrono's
/// range — a tampered cursor, never a 500).
#[derive(Debug, thiserror::Error)]
pub enum AttestationReviewError {
    /// The `?flagged=` value is not `true` or `false`.
    #[error("invalid flagged filter")]
    InvalidFlagged,
    /// The cursor does not decode (or its `ts` is outside
    /// chrono's range — a tampered cursor, never a 500).
    #[error("invalid pagination cursor")]
    InvalidCursor,
    /// A database read failed.
    #[error("attestation review failed")]
    Internal,
}

/// Parse the `?flagged=` filter: `true` → flagged only,
/// `false` → unflagged only, absent → all rows. Any other
/// value is a client error, not an empty page (the P1-090
/// `?status=` precedent — a list filter that names no
/// state is a 422).
pub fn parse_flagged(raw: Option<&str>) -> Result<Option<bool>, AttestationReviewError> {
    match raw {
        None => Ok(None),
        Some("true") => Ok(Some(true)),
        Some("false") => Ok(Some(false)),
        Some(_) => Err(AttestationReviewError::InvalidFlagged),
    }
}

/// List one page of the review queue: the optional flagged
/// filter, the clamped limit, the decoded cursor. Fetches
/// `limit + 1` rows so the page knows whether a next page
/// exists. A read writes no audit row (P1-046).
pub async fn list_attestations(
    state: &AppState,
    flagged: Option<String>,
    limit: Option<u32>,
    cursor: Option<String>,
) -> Result<AttestationsPage, AttestationReviewError> {
    let flagged = parse_flagged(flagged.as_deref())?;
    let limit = clamp_limit(limit);
    let cursor = match cursor {
        Some(raw) => {
            let decoded =
                Cursor::decode(&raw).map_err(|_| AttestationReviewError::InvalidCursor)?;
            // The same out-of-range `ts` guard as the audit
            // query (P1-088): a decoded cursor can carry a
            // `ts` outside chrono's range, and the repo's
            // `from_timestamp_millis(...).expect(...)` would
            // panic on it — out of range is a tampered
            // cursor (422), never a 500.
            DateTime::from_timestamp_millis(decoded.ts)
                .ok_or(AttestationReviewError::InvalidCursor)?;
            Some(decoded)
        }
        None => None,
    };
    let rows =
        attestations::list_filtered(&state.pool, flagged, VELOCITY_THRESHOLD, cursor, limit + 1)
            .await
            .map_err(|err| {
                tracing::error!(error = ?err, "attestation review: list failed");
                AttestationReviewError::Internal
            })?;
    let page = Page::from_window(rows, limit, |row| Cursor {
        ts: row.created_at.timestamp_millis(),
        id: row.id,
    });
    Ok(AttestationsPage {
        items: page
            .items
            .into_iter()
            .map(AttestationItem::from_row)
            .collect(),
        next_cursor: page.next_cursor,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn row_at(velocity_count: i64) -> AttestationRow {
        AttestationRow {
            id: Uuid::now_v7(),
            user_id: Uuid::now_v7(),
            upload_id: Uuid::now_v7(),
            text_version: "v1".to_string(),
            ip_hash: "a".repeat(64),
            client: "web".to_string(),
            created_at: Utc.with_ymd_and_hms(2026, 11, 19, 12, 0, 0).unwrap(),
            velocity_count,
        }
    }

    #[test]
    fn velocity_flag_boundary_is_strict() {
        // Exactly the threshold is not an anomaly.
        assert!(flag_reasons_for(VELOCITY_THRESHOLD).is_empty());
        assert!(flag_reasons_for(VELOCITY_THRESHOLD - 1).is_empty());
        // One over is.
        assert_eq!(flag_reasons_for(VELOCITY_THRESHOLD + 1), vec!["velocity"]);
    }

    #[test]
    fn item_carries_the_flag_and_the_link() {
        let user_id = Uuid::now_v7();
        let mut row = row_at(VELOCITY_THRESHOLD + 1);
        row.user_id = user_id;
        let item = AttestationItem::from_row(row);
        assert!(item.flagged);
        assert_eq!(item.flag_reasons, vec!["velocity"]);
        assert_eq!(item.velocity_count, VELOCITY_THRESHOLD + 1);
        assert_eq!(
            item.abuse_queue_url,
            format!("/v1/admin/abuse-reports?user_id={user_id}")
        );
        let json = serde_json::to_value(&item).expect("serializes");
        assert_eq!(json["flagged"], true);
        assert_eq!(json["flag_reasons"], serde_json::json!(["velocity"]));
        assert_eq!(json["text_version"], "v1");
        assert_eq!(json["client"], "web");
    }

    #[test]
    fn unflagged_item_has_empty_reasons() {
        let item = AttestationItem::from_row(row_at(1));
        assert!(!item.flagged);
        assert!(item.flag_reasons.is_empty());
        let json = serde_json::to_value(&item).expect("serializes");
        assert_eq!(json["flag_reasons"], serde_json::json!([]));
    }

    #[test]
    fn parse_flagged_accepts_only_true_false_or_absent() {
        assert_eq!(parse_flagged(None).expect("absent"), None);
        assert_eq!(parse_flagged(Some("true")).expect("true"), Some(true));
        assert_eq!(parse_flagged(Some("false")).expect("false"), Some(false));
        assert!(parse_flagged(Some("yes")).is_err());
        assert!(parse_flagged(Some("1")).is_err());
        assert!(parse_flagged(Some("TRUE")).is_err());
        assert!(parse_flagged(Some("")).is_err());
    }

    #[test]
    fn abuse_queue_url_is_a_relative_reference() {
        let user_id = Uuid::now_v7();
        let url = abuse_queue_url(user_id);
        assert!(!url.starts_with("http"), "same-origin, no host: {url}");
        assert_eq!(url, format!("/v1/admin/abuse-reports?user_id={user_id}"));
    }
}
