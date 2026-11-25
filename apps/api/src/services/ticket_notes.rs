//! Ticket notes (P1-110, FR-C-02): `POST
//! /v1/admin/users/{id}/notes` — an admin appends a note to a
//! user's append-only list, audited as
//! `admin.users.note_added`.
//!
//! A note is one JSON object in `users.support_notes`
//! (migration 20261124210000): `{id (uuidv7), author_id,
//! author_role, body, created_at}`. The id + `created_at`
//! come from the app clock (the P1-107 frozen-clock
//! precedent); `author_role` is recorded at write time (roles
//! can change; the history must say who the author was then,
//! not now). The list is append-only twice over: one repo fn
//! appends (`support_notes || $1::jsonb`, RETURNING the list)
//! and no repo fn replaces the column, no endpoint edits or
//! deletes — and the `users_support_notes_append_only`
//! trigger raises on any UPDATE that drops or rewrites an
//! existing element. A wrong note is corrected by appending
//! a new one, so the history stays honest.
//!
//! The note and its audit row commit in ONE transaction (the
//! P1-008 atomicity rule — the row and the note commit
//! together or not at all), and the handler marks the
//! response `AuditHandled` so the P1-046 middleware stands
//! down and the row exists exactly once. The audit row
//! carries NO verbatim body (after `{note_id, body_chars}`)
//! — the note lives in exactly one place (the column); the
//! audit proves who appended to whom and when, it must not
//! become a second copy of support's working text.
//!
//! Support READS the notes through the P1-106 view (a sixth
//! section, `support_notes` — the full list, oldest first):
//! the support flow already opens the view for every user,
//! so there is no second GET route and no second shape. A
//! support-authored note is NOT this ticket — support reads,
//! admin writes.
//!
//! The write is admin-only ([`AdminContext`] — support is
//! read-only, so support gets 403 `PF-1002` from the
//! extractor, an API key 403, no credential 401 `PF-1001`).
//! A user who does not exist is 404 `PF-2010` (the P1-081
//! precedent). Any status may be noted: a note on a suspended
//! or deletion-pending user is exactly when support needs to
//! record why (ASSUMPTION-071). The handler answers 200
//! with the full list after the append (an admin action on a
//! user, not a 201 — the suspend/warn/grant precedent).

use crate::AppState;
use chrono::{DateTime, Utc};
use detag_core::{
    audit::{record, NewAuditLog},
    db::PoolConfig,
    error::FieldError,
    repos::{
        users::{self, User},
        RepoError,
    },
    tx::{self, TxError},
};
use detag_validator::{Validate, ValidationErrors};
use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;
use utoipa::ToSchema;
use uuid::Uuid;

/// The longest a note body may be, in characters: a note is
/// a paragraph, not a reason (the P1-008 reason cap is 500)
/// — 2000 chars is ~300 words, enough for "called the user,
/// they will re-upload on Monday" without becoming a letter.
pub const MAX_NOTE_BODY_LEN: usize = 2000;

/// The audit action for a note append (the matrix maps the
/// route to it — `Capability::audit_action_for`).
pub const NOTE_ADDED_AUDIT_ACTION: &str = "admin.users.note_added";

/// What appending a note can fail with.
#[derive(Debug, Error)]
pub enum TicketNoteError {
    #[error("ticket note validation failed")]
    Validation(ValidationErrors),
    #[error("user not found")]
    NotFound,
    #[error("ticket note failed")]
    Internal,
}

/// The transaction closure's error: small on purpose — it
/// must not recurse into [`TicketNoteError`].
#[derive(Debug)]
enum TicketNoteTxError {
    Db(RepoError),
}

fn map_tx_error(context: &'static str) -> impl Fn(TxError<TicketNoteTxError>) -> TicketNoteError {
    move |err| match err {
        TxError::Inner(TicketNoteTxError::Db(db_err)) => {
            tracing::error!(error = ?db_err, "{context}: transaction failed (db)");
            TicketNoteError::Internal
        }
        other => {
            tracing::error!(error = ?other, "{context}: transaction failed");
            TicketNoteError::Internal
        }
    }
}

/// Validate a note body: trim it, then it must be non-empty
/// and at most [`MAX_NOTE_BODY_LEN`] characters, with no
/// control characters (the P1-008 `validate_reason` rules
/// with a longer cap — a note is a paragraph, not a reason).
/// Returns the TRIMMED value on success (the caller stores
/// the trimmed form — leading / trailing spaces are never
/// stored).
pub fn validate_note_body(field: &str, value: &str) -> Result<String, FieldError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(FieldError::new(field, "required", "is required"));
    }
    if trimmed.chars().count() > MAX_NOTE_BODY_LEN {
        return Err(FieldError::new(
            field,
            "too_long",
            "must be at most 2000 characters",
        ));
    }
    if trimmed.chars().any(char::is_control) {
        return Err(FieldError::new(
            field,
            "invalid_body",
            "must not contain control characters",
        ));
    }
    Ok(trimmed.to_string())
}

/// One ticket note: the JSON object stored in
/// `users.support_notes`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TicketNote {
    /// The note id (uuidv7, from the app clock).
    #[schema(value_type = String, example = "123e4567-e89b-42d3-a456-426614174109")]
    pub id: Uuid,
    /// Who wrote it.
    #[schema(value_type = String, example = "123e4567-e89b-42d3-a456-426614174109")]
    pub author_id: Uuid,
    /// The author's role at write time (`admin` — only
    /// admins can write today, recorded anyway so the history
    /// survives a role change).
    pub author_role: String,
    /// The note text (1–2000 chars, trimmed).
    pub body: String,
    /// When it was written (the app clock).
    pub created_at: DateTime<Utc>,
}

/// Parse one stored note JSON value into a [`TicketNote`].
/// `None` when the stored JSON is not a note (corrupt data —
/// only this service writes the column, so this is a 500,
/// never a user error).
pub fn parse_stored_note(value: serde_json::Value) -> Option<TicketNote> {
    serde_json::from_value(value).ok()
}

/// The note request: the body, nothing else (the target is
/// the path id, the author is the caller).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TicketNoteRequest {
    /// The note text (1–2000 characters after trimming).
    pub body: String,
}

impl Validate for TicketNoteRequest {
    fn validate(&self) -> Result<(), ValidationErrors> {
        let mut errors = ValidationErrors::default();
        if let Err(err) = validate_note_body("body", &self.body) {
            errors.push(err);
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

/// The notes, as the API returns them: the user + the full
/// list after the append, oldest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct TicketNotesResponse {
    /// Whose notes these are.
    #[schema(value_type = String, example = "123e4567-e89b-42d3-a456-426614174109")]
    pub user_id: Uuid,
    /// The full list, oldest first (the appended note is
    /// last).
    pub notes: Vec<TicketNote>,
}

/// Build the audit row for one note append (pure — the unit
/// test pins the shape). The row carries the note id + the
/// body length only — NO verbatim body (the note lives in
/// exactly one place: the column).
pub fn note_audit_row(admin: &User, target: &User, note: &TicketNote) -> NewAuditLog {
    NewAuditLog {
        actor_user_id: Some(admin.id),
        actor_client: None,
        action: NOTE_ADDED_AUDIT_ACTION.to_string(),
        target_type: "user".to_string(),
        target_id: Some(target.id.to_string()),
        before: None,
        after: Some(json!({
            "note_id": note.id,
            "body_chars": note.body.chars().count(),
        })),
        reason: None,
    }
}

/// Append a ticket note to a user (P1-110). One transaction:
/// the `support_notes` append + the `admin.users.note_added`
/// audit row. The note id + stamp come from the app clock,
/// so a frozen clock pins them in tests.
pub async fn append_ticket_note(
    state: &AppState,
    admin: &User,
    target_id: Uuid,
    req: &TicketNoteRequest,
) -> Result<TicketNotesResponse, TicketNoteError> {
    req.validate().map_err(TicketNoteError::Validation)?;
    let target = users::by_id(&state.pool, target_id).await.map_err(|err| {
        tracing::error!(error = ?err, "ticket note: user lookup failed");
        TicketNoteError::Internal
    })?;
    let Some(target) = target else {
        return Err(TicketNoteError::NotFound);
    };
    let now = state.clock.now();
    // The trimmed body: `req.validate()` already passed, so
    // this cannot fail — but a request path never panics, so
    // a failure here is a 422, not an `expect`.
    let body = match validate_note_body("body", &req.body) {
        Ok(body) => body,
        Err(err) => {
            let mut errors = ValidationErrors::default();
            errors.push(err);
            return Err(TicketNoteError::Validation(errors));
        }
    };
    let note = TicketNote {
        id: Uuid::now_v7(),
        author_id: admin.id,
        author_role: admin.role.clone(),
        body,
        created_at: now,
    };
    let timeout = PoolConfig::default().transaction_timeout;
    // The note JSON is built before the transaction (the
    // `json!` macro cannot fail — the struct's fields are all
    // serializable — so no error mapping is needed inside the
    // closure).
    let note_json = json!({
        "id": note.id,
        "author_id": note.author_id,
        "author_role": note.author_role,
        "body": note.body,
        "created_at": note.created_at,
    });
    let stored = tx::with_tx(&state.pool, timeout, |conn| {
        let admin = admin.clone();
        let target = target.clone();
        let note = note.clone();
        let note_json = note_json.clone();
        Box::pin(async move {
            let stored = users::append_support_note(&mut *conn, target.id, &note_json)
                .await
                .map_err(TicketNoteTxError::Db)?;
            record(&mut *conn, &note_audit_row(&admin, &target, &note))
                .await
                .map_err(TicketNoteTxError::Db)?;
            Ok(stored)
        })
    })
    .await
    .map_err(map_tx_error("ticket note"))?;
    let mut notes = Vec::with_capacity(stored.len());
    for value in stored {
        match parse_stored_note(value) {
            Some(note) => notes.push(note),
            None => {
                tracing::error!("ticket note: stored note failed to parse");
                return Err(TicketNoteError::Internal);
            }
        }
    }
    Ok(TicketNotesResponse {
        user_id: target.id,
        notes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn admin() -> User {
        User {
            id: Uuid::parse_str("123e4567-e89b-42d3-a456-426614174109").unwrap(),
            email: "admin@example.com".to_string(),
            password_hash: "$argon2id$v=19$m=19456,t=2,p=1$SECRET$SECRET".to_string(),
            email_verified_at: Some(Utc.with_ymd_and_hms(2026, 11, 19, 12, 0, 0).unwrap()),
            role: "admin".to_string(),
            status: "active".to_string(),
            tos_version: Some("v1".to_string()),
            aup_version: Some("v1".to_string()),
            deleted_at: None,
            revoked_before: None,
            display_name: Some("Admin".to_string()),
            pending_email: None,
            settings: serde_json::json!({}),
            created_at: Utc.with_ymd_and_hms(2026, 11, 19, 10, 0, 0).unwrap(),
            updated_at: Utc.with_ymd_and_hms(2026, 11, 19, 10, 0, 0).unwrap(),
        }
    }

    fn target() -> User {
        let mut user = admin();
        user.id = Uuid::parse_str("123e4567-e89b-42d3-a456-426614174110").unwrap();
        user.email = "user@example.com".to_string();
        user.role = "user".to_string();
        user
    }

    fn note() -> TicketNote {
        TicketNote {
            id: Uuid::parse_str("123e4567-e89b-42d3-a456-426614174111").unwrap(),
            author_id: admin().id,
            author_role: "admin".to_string(),
            body: "Called the user, they will re-upload on Monday.".to_string(),
            created_at: Utc.with_ymd_and_hms(2026, 11, 19, 12, 0, 0).unwrap(),
        }
    }

    #[test]
    fn valid_body_passes_and_trims() {
        let req = TicketNoteRequest {
            body: "  Called the user.  ".to_string(),
        };
        assert!(req.validate().is_ok());
        assert_eq!(
            validate_note_body("body", "  Called the user.  ").unwrap(),
            "Called the user."
        );
        // Exactly 2000 chars passes; 2001 fails.
        let body = "a".repeat(2000);
        assert!(validate_note_body("body", &body).is_ok());
        let body = "a".repeat(2001);
        let err = validate_note_body("body", &body).unwrap_err();
        assert_eq!(err.field, "body");
        assert_eq!(err.code, "too_long");
    }

    #[test]
    fn blank_and_control_body_are_422() {
        let req = TicketNoteRequest {
            body: "   ".to_string(),
        };
        let err = req.validate().unwrap_err();
        assert_eq!(err.len(), 1);
        assert_eq!(err.0[0].field, "body");
        assert_eq!(err.0[0].code, "required");
        // A control character (tab inside the body) fails —
        // and the error names the field, not the content.
        let req = TicketNoteRequest {
            body: "has\ta tab".to_string(),
        };
        let err = req.validate().unwrap_err();
        assert_eq!(err.len(), 1);
        assert_eq!(err.0[0].field, "body");
        assert_eq!(err.0[0].code, "invalid_body");
    }

    #[test]
    fn note_audit_row_carries_id_and_length_but_not_the_body() {
        let row = note_audit_row(&admin(), &target(), &note());
        assert_eq!(row.action, "admin.users.note_added");
        assert_eq!(row.actor_user_id, Some(admin().id));
        assert_eq!(row.target_type, "user");
        assert_eq!(
            row.target_id.as_deref(),
            Some(target().id.to_string()).as_deref()
        );
        assert!(row.before.is_none(), "an append has no before state");
        assert!(row.reason.is_none(), "a note append carries no reason");
        let after = row.after.as_ref().unwrap();
        assert_eq!(after["note_id"].as_str().unwrap(), note().id.to_string());
        assert_eq!(after["body_chars"], 47);
        // The no-verbatim-body rule: the note text is nowhere
        // in the row — the after payload is the only place
        // text could hide, so grep it.
        let json = serde_json::to_string(&after).unwrap();
        assert!(
            !json.contains("re-upload"),
            "audit row must not carry the note body: {json}"
        );
        // The audit service's own shape rule accepts the row.
        detag_core::audit::validate(&row).expect("note row is valid");
    }

    #[test]
    fn stored_note_round_trips_through_json() {
        // The repo stores the note as one JSON object; the
        // parse must read back exactly what was written —
        // including the author role recorded at write time.
        let value = serde_json::to_value(note()).unwrap();
        let parsed = parse_stored_note(value).expect("a note parses");
        assert_eq!(parsed, note());
        // A stored value that is not a note parses to None
        // (corrupt data is a 500, never a user error).
        assert!(parse_stored_note(json!({"body": "no id"})).is_none());
        assert!(parse_stored_note(json!("just a string")).is_none());
    }
}
