//! User lookup helper (P1-109, FR-C-02): `GET
//! /v1/admin/users/lookup?email=…` / `?id=…` / `?job_id=…` —
//! resolve a user from whatever identifier a support ticket
//! carries, and return the P1-106 support view.
//!
//! The lookup resolves to a user id, then calls
//! [`get_admin_user_view`](super::admin_user_view::get_admin_user_view)
//! — NO second shape, NO second secret-exclusion audit (the
//! view already excludes `password_hash`/`settings`/
//! `revoked_before`, provider ids, job `params`/
//! `webhook_url`/`idempotency_key`/`error`, session
//! `refresh_hash`/`family_id`, pinned by the P1-106 unit +
//! live shape tests). A job-id lookup resolves through the
//! job's `user_id` and returns the USER, not the job.
//!
//! Every lookup audits as `admin.users.lookup` — including
//! misses: a miss (no such email / id / job) is 404
//! `PF-2010` AND still writes one row (target `null`, after
//! `{lookup_kind, found: false}`) — a probing pattern is
//! visible only if misses are logged. The row is written by
//! the service via `record()` (the P1-108 precedent); the
//! route is deliberately NOT in `audit_action_for` (the
//! P1-046 middleware's contract is writes; its
//! `target_id_from` reads the path, and a query-param lookup
//! has no path id). NO raw searched value is stored in the
//! audit row (no email, no job id — the resolved user id
//! identifies the access). Denials (401/403) and validation
//! failures (422) audit NOTHING (the extractor rejects
//! before the handler runs; a 422 means no lookup
//! happened). No state change, no provider call, no mail.
//!
//! Deleted users follow the view: `users::by_id` carries no
//! `deleted_at` filter, so a deletion-pending user still
//! resolves and the view carries `deleted_at` in the
//! profile — a deleted user is a user with a deletion time,
//! not a missing one (verified against source, P1-109).
//! The handler answers 200 (a read, the P1-106 precedent),
//! not 201.
//!
//! Admin + support ([`AdminReadContext`](crate::auth::AdminReadContext)
//! — the P1-106 guard); anon 401 `PF-1001`, plain user 403
//! `PF-1002`, API key 403 (the P1-106 precedent — "no admin
//! ever").

use crate::AppState;
use detag_core::{
    audit::{record, NewAuditLog},
    error::FieldError,
    repos::{jobs, users, users::User},
};
use detag_validator::{validate_email, ValidationErrors};
use serde::Deserialize;
use serde_json::json;
use thiserror::Error;
use uuid::Uuid;

use super::admin_user_view::{get_admin_user_view, AdminUserViewError, AdminUserViewResponse};

/// The audit action for a lookup (the route is deliberately
/// NOT in `audit_action_for` — see the module docs — so the
/// service writes this row itself, exactly once per call
/// that reaches it).
pub const USER_LOOKUP_AUDIT_ACTION: &str = "admin.users.lookup";

/// What a user lookup can fail with.
#[derive(Debug, Error)]
pub enum UserLookupError {
    #[error("user lookup validation failed")]
    Validation(ValidationErrors),
    #[error("no such user")]
    NotFound,
    #[error("user lookup failed")]
    Internal,
}

/// Which key the caller looked up by. The `after` payload
/// of the audit row carries this, never the searched value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LookupKind {
    Email,
    Id,
    JobId,
}

impl LookupKind {
    /// The audit payload's spelling (`"email" | "id" |
    /// `"job_id"`).
    pub fn as_str(&self) -> &'static str {
        match self {
            LookupKind::Email => "email",
            LookupKind::Id => "id",
            LookupKind::JobId => "job_id",
        }
    }
}

/// The resolved key: exactly one of the three, already
/// shape-checked (a parsed email string, a parsed UUID).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LookupKey {
    Email(String),
    Id(Uuid),
    JobId(Uuid),
}

impl LookupKey {
    /// Which key this is (for the audit row).
    pub fn kind(&self) -> LookupKind {
        match self {
            LookupKey::Email(_) => LookupKind::Email,
            LookupKey::Id(_) => LookupKind::Id,
            LookupKey::JobId(_) => LookupKind::JobId,
        }
    }
}

/// The lookup query: exactly one of `email` / `id` /
/// `job_id`. Unknown keys are 422 (`deny_unknown_fields` —
/// a typo'd key must not silently resolve nothing).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LookupQuery {
    pub email: Option<String>,
    pub id: Option<String>,
    pub job_id: Option<String>,
}

/// Parse + validate the lookup query: exactly one of the
/// three keys must be present, and the present key must
/// parse (email shape for `email`, UUID for `id` /
/// `job_id`). All failures are collected, not just the
/// first — a two-key query with a bad email reports both
/// the exactly-one error and the shape error.
pub fn parse_lookup_query(query: &LookupQuery) -> Result<LookupKey, ValidationErrors> {
    let mut errors = ValidationErrors::default();
    let present =
        query.email.is_some() as u8 + query.id.is_some() as u8 + query.job_id.is_some() as u8;
    if present != 1 {
        errors.push(FieldError::new(
            "query",
            if present == 0 {
                "missing_key"
            } else {
                "too_many_keys"
            },
            "exactly one of email, id, job_id is required",
        ));
    }
    if let Some(email) = &query.email {
        if let Err(err) = validate_email("email", email) {
            errors.push(err);
        }
    }
    let mut id = None;
    if let Some(raw) = &query.id {
        match Uuid::parse_str(raw) {
            Ok(parsed) => id = Some(parsed),
            Err(_) => errors.push(FieldError::new(
                "id",
                "invalid_uuid",
                "id must be a valid UUID",
            )),
        }
    }
    let mut job_id = None;
    if let Some(raw) = &query.job_id {
        match Uuid::parse_str(raw) {
            Ok(parsed) => job_id = Some(parsed),
            Err(_) => errors.push(FieldError::new(
                "job_id",
                "invalid_uuid",
                "job_id must be a valid UUID",
            )),
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    // Exactly one key is present and it parsed (the errors
    // above cover every other shape), so one arm matches.
    if let Some(email) = &query.email {
        Ok(LookupKey::Email(email.clone()))
    } else if let Some(parsed) = id {
        Ok(LookupKey::Id(parsed))
    } else if let Some(parsed) = job_id {
        Ok(LookupKey::JobId(parsed))
    } else {
        // Unreachable: `present == 1` with no error means
        // one key parsed — but defended anyway, never a
        // panic on a request path.
        Err(ValidationErrors(vec![FieldError::new(
            "query",
            "missing_key",
            "exactly one of email, id, job_id is required",
        )]))
    }
}

/// Build the audit row for one lookup (pure — the unit
/// test pins the shape). `target` is the resolved user id
/// (`None` on a miss); `found` is whether the lookup
/// resolved. The row carries the kind + found only — NO
/// raw searched value (no email, no job id): the resolved
/// user id identifies the access, and the searched value
/// stays in the request, not the log.
pub fn lookup_audit_row(
    caller: &User,
    kind: LookupKind,
    target: Option<Uuid>,
    found: bool,
) -> NewAuditLog {
    NewAuditLog {
        actor_user_id: Some(caller.id),
        actor_client: None,
        action: USER_LOOKUP_AUDIT_ACTION.to_string(),
        target_type: "user".to_string(),
        target_id: target.map(|id| id.to_string()),
        before: None,
        after: Some(json!({
            "lookup_kind": kind.as_str(),
            "found": found,
        })),
        reason: None,
    }
}

/// Look a user up by email / id / job id and return the
/// P1-106 view. Exactly one `admin.users.lookup` audit row
/// is written per call that reaches the service — a hit
/// (target the resolved user, after `{lookup_kind, found:
/// true}`) or a miss (404 `PF-2010`, target `null`, after
/// `{lookup_kind, found: false}`). No state change, no
/// provider call, no mail.
pub async fn lookup_user(
    state: &AppState,
    caller: &User,
    query: &LookupQuery,
) -> Result<AdminUserViewResponse, UserLookupError> {
    let key = parse_lookup_query(query).map_err(UserLookupError::Validation)?;
    let kind = key.kind();
    // Resolve the key to a user id. A miss is a 404 with
    // its audit row, not an empty view (the P1-081
    // precedent — the caller asked about a specific user).
    let user_id = match &key {
        LookupKey::Email(email) => users::by_email(&state.pool, email)
            .await
            .map_err(|err| {
                tracing::error!(error = ?err, "user lookup: email resolve failed");
                UserLookupError::Internal
            })?
            .map(|user| user.id),
        LookupKey::Id(id) => {
            let exists = users::by_id(&state.pool, *id)
                .await
                .map_err(|err| {
                    tracing::error!(error = ?err, "user lookup: id resolve failed");
                    UserLookupError::Internal
                })?
                .is_some();
            exists.then_some(*id)
        }
        LookupKey::JobId(job_id) => jobs::by_id(&state.pool, *job_id)
            .await
            .map_err(|err| {
                tracing::error!(error = ?err, "user lookup: job resolve failed");
                UserLookupError::Internal
            })?
            .map(|job| job.user_id),
    };
    let Some(user_id) = user_id else {
        record(&state.pool, &lookup_audit_row(caller, kind, None, false))
            .await
            .map_err(|err| {
                tracing::error!(error = ?err, "user lookup: miss audit write failed");
                UserLookupError::Internal
            })?;
        return Err(UserLookupError::NotFound);
    };
    match get_admin_user_view(state, user_id).await {
        Ok(view) => {
            record(
                &state.pool,
                &lookup_audit_row(caller, kind, Some(user_id), true),
            )
            .await
            .map_err(|err| {
                tracing::error!(error = ?err, "user lookup: audit write failed");
                UserLookupError::Internal
            })?;
            Ok(view)
        }
        // The user vanished between the resolve and the
        // view (a hard-delete race — deletion itself is
        // soft, so this is a torn read, not a rule): the
        // honest answer is the same miss the resolve
        // would have given, with the same miss row.
        Err(AdminUserViewError::NotFound) => {
            record(&state.pool, &lookup_audit_row(caller, kind, None, false))
                .await
                .map_err(|err| {
                    tracing::error!(error = ?err, "user lookup: miss audit write failed");
                    UserLookupError::Internal
                })?;
            Err(UserLookupError::NotFound)
        }
        Err(err) => {
            if let AdminUserViewError::Db(db_err) = &err {
                tracing::error!(error = %db_err, "user lookup: view read failed");
            }
            Err(UserLookupError::Internal)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn caller() -> User {
        User {
            id: Uuid::parse_str("123e4567-e89b-42d3-a456-426614174109").unwrap(),
            email: "support@example.com".to_string(),
            password_hash: "$argon2id$v=19$m=19456,t=2,p=1$SECRET$SECRET".to_string(),
            email_verified_at: Some(Utc.with_ymd_and_hms(2026, 11, 19, 12, 0, 0).unwrap()),
            role: "support".to_string(),
            status: "active".to_string(),
            tos_version: Some("v1".to_string()),
            aup_version: Some("v1".to_string()),
            deleted_at: None,
            revoked_before: None,
            display_name: Some("Support".to_string()),
            pending_email: None,
            settings: serde_json::json!({}),
            created_at: Utc.with_ymd_and_hms(2026, 11, 19, 10, 0, 0).unwrap(),
            updated_at: Utc.with_ymd_and_hms(2026, 11, 19, 10, 0, 0).unwrap(),
        }
    }

    fn query(email: Option<&str>, id: Option<&str>, job_id: Option<&str>) -> LookupQuery {
        LookupQuery {
            email: email.map(str::to_string),
            id: id.map(str::to_string),
            job_id: job_id.map(str::to_string),
        }
    }

    #[test]
    fn parse_lookup_query_accepts_each_key_alone_and_rejects_the_rest() {
        // Each key alone parses to its kind.
        assert_eq!(
            parse_lookup_query(&query(Some("alice@example.com"), None, None)).unwrap(),
            LookupKey::Email("alice@example.com".to_string())
        );
        assert_eq!(
            parse_lookup_query(&query(
                None,
                Some("123e4567-e89b-42d3-a456-426614174109"),
                None
            ))
            .unwrap(),
            LookupKey::Id(Uuid::parse_str("123e4567-e89b-42d3-a456-426614174109").unwrap())
        );
        assert_eq!(
            parse_lookup_query(&query(
                None,
                None,
                Some("123e4567-e89b-42d3-a456-426614174110")
            ))
            .unwrap(),
            LookupKey::JobId(Uuid::parse_str("123e4567-e89b-42d3-a456-426614174110").unwrap())
        );
        // Zero keys: exactly the missing-key error.
        let errors = parse_lookup_query(&query(None, None, None)).unwrap_err();
        assert_eq!(errors.len(), 1, "zero keys is one error: {errors:?}");
        assert_eq!(errors.0[0].field, "query");
        assert_eq!(errors.0[0].code, "missing_key");
        // Two keys: the too-many error — and nothing else
        // (both values parse, so no shape error joins it).
        let errors = parse_lookup_query(&query(
            Some("alice@example.com"),
            Some("123e4567-e89b-42d3-a456-426614174109"),
            None,
        ))
        .unwrap_err();
        assert_eq!(errors.len(), 1, "two valid keys is one error: {errors:?}");
        assert_eq!(errors.0[0].field, "query");
        assert_eq!(errors.0[0].code, "too_many_keys");
        // All failures collected: two keys + a bad email
        // reports the exactly-one error AND the shape error.
        let errors = parse_lookup_query(&query(
            Some("not-an-email"),
            Some("123e4567-e89b-42d3-a456-426614174109"),
            None,
        ))
        .unwrap_err();
        assert_eq!(
            errors.len(),
            2,
            "two keys + bad email is two errors: {errors:?}"
        );
        assert!(errors.0.iter().any(|e| e.field == "query"));
        assert!(errors.0.iter().any(|e| e.field == "email"));
        // Bad shapes, one key each: the field + code name
        // the bad field, and the searched value is never in
        // the error (a UUID typo is not echoed back).
        let errors = parse_lookup_query(&query(Some("not-an-email"), None, None)).unwrap_err();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors.0[0].field, "email");
        let errors = parse_lookup_query(&query(None, Some("not-a-uuid"), None)).unwrap_err();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors.0[0].field, "id");
        assert_eq!(errors.0[0].code, "invalid_uuid");
        let errors = parse_lookup_query(&query(None, None, Some("not-a-uuid"))).unwrap_err();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors.0[0].field, "job_id");
        assert_eq!(errors.0[0].code, "invalid_uuid");
        // An empty email counts as present (the key is
        // there) and fails the shape rule, not the count.
        let errors = parse_lookup_query(&query(Some(""), None, None)).unwrap_err();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors.0[0].field, "email");
    }

    #[test]
    fn lookup_audit_row_carries_kind_and_found_and_no_searched_value() {
        // A hit: actor the caller, target the resolved user,
        // after the kind + found — and nothing else.
        let target = Uuid::parse_str("123e4567-e89b-42d3-a456-426614174110").unwrap();
        let row = lookup_audit_row(&caller(), LookupKind::Email, Some(target), true);
        assert_eq!(row.action, "admin.users.lookup");
        assert_eq!(row.actor_user_id, Some(caller().id));
        assert_eq!(row.target_type, "user");
        assert_eq!(
            row.target_id.as_deref(),
            Some(target.to_string()).as_deref()
        );
        assert!(row.before.is_none(), "a lookup has no before state");
        assert!(row.reason.is_none(), "a lookup carries no reason");
        let after = row.after.as_ref().unwrap();
        assert_eq!(after["lookup_kind"], "email");
        assert_eq!(after["found"], true);
        // The no-raw-value rule: the searched email is
        // nowhere in the row — the after payload is the only
        // place a value could hide (the other fields are ids
        // + the action), so grep it: a value added to `after`
        // later fails here too. (`NewAuditLog` carries no
        // Serialize, so the grep runs on the payload, not
        // the row.)
        let json = serde_json::to_string(&after).unwrap();
        assert!(
            !json.contains("alice@example.com"),
            "audit row must not carry the searched email: {json}"
        );
        // A miss: target null, found false — and the row is
        // still well-formed (the audit service accepts a
        // null target id).
        let row = lookup_audit_row(&caller(), LookupKind::JobId, None, false);
        assert_eq!(row.target_id, None, "a miss has no target");
        assert_eq!(row.after.as_ref().unwrap()["lookup_kind"], "job_id");
        assert_eq!(row.after.as_ref().unwrap()["found"], false);
        assert_eq!(row.action, "admin.users.lookup");
        assert_eq!(row.actor_user_id, Some(caller().id));
        // The audit service's own shape rule accepts both
        // rows (dotted action, non-empty target type).
        detag_core::audit::validate(&row).expect("miss row is valid");
        let row = lookup_audit_row(&caller(), LookupKind::Email, Some(target), true);
        detag_core::audit::validate(&row).expect("hit row is valid");
    }
}
