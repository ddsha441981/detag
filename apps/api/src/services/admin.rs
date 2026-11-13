//! Admin user actions (P1-008): suspend + restore.
//!
//! - [`suspend_user`] — status `suspended`, every live session
//!   revoked, every queued job held, `user.suspended` audited
//!   with the admin's reason. A suspend cancels any pending
//!   deletion (`deleted_at` is cleared — the grace does not
//!   survive admin intervention; the audit's `before` records
//!   the prior status + `deleted_at`).
//! - [`restore_user`] — status `active`, held jobs released
//!   back to `queued`, `user.restored` audited. Sessions stay
//!   revoked (revocation is permanent — the user logs in
//!   again); restore does NOT re-run a deletion.
//!
//! Both are idempotent: suspending an already-suspended
//! account (or restoring a non-suspended one) is a 200 no-op —
//! no write, no audit, no second mail. The caller is an admin
//! (the [`AdminContext`] extractor guarantees it); the target
//! is any user, including another admin (no hierarchy is
//! defined — suspending the last admin is an owner question,
//! not a code guard; see DECISIONS.md if it ever matters).
//!
//! What suspend does NOT do: it does not kill the access
//! token already issued — the token is stateless and stays
//! valid until it expires (≤ 15 min, the documented residual
//! window). It kills the refresh sessions (no new access
//! tokens) and blocks the next login (a suspended account's
//! login is 403 `PF-1003` after a correct password — see
//! `services/login.rs`). It does not hold a job already
//! running — only `queued` jobs move to `held`; the worker
//! finishes the in-flight one.

use crate::AppState;
use chrono::{DateTime, Utc};
use detag_core::{
    db::PoolConfig,
    pagination::{clamp_limit, Cursor, Page},
    repos::{
        audit_logs::{self, NewAuditLog},
        jobs, sessions,
        users::{self, User, UserSearchRow},
        RepoError,
    },
    tx::{self, TxError},
};
use detag_validator::{validate_reason, Validate, ValidationErrors};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use utoipa::ToSchema;
use uuid::Uuid;

/// What suspend/restore can fail with.
#[derive(Debug, Error)]
pub enum AdminError {
    #[error("admin validation failed")]
    Validation(ValidationErrors),
    #[error("user not found")]
    NotFound,
    #[error("invalid pagination cursor")]
    InvalidCursor,
    #[error("admin action failed")]
    Internal,
}

/// The transaction closure's error: small on purpose — it must
/// not recurse into [`AdminError`].
#[derive(Debug)]
enum AdminTxError {
    Db(RepoError),
}

/// The outcome of a suspend/restore: the updated user + what
/// the transition did. The counts are 0 on a no-op (`already`
/// is true) and 0 for the action that does not use them
/// (suspend never releases, restore never revokes).
#[derive(Debug)]
pub struct AdminActionOutcome {
    /// The user after the transition (or as it already was, on
    /// a no-op).
    pub user: User,
    /// The account was already in the target state — no write,
    /// no audit.
    pub already: bool,
    /// Sessions revoked (suspend only).
    pub sessions_revoked: u64,
    /// Queued jobs held (suspend only).
    pub jobs_held: u64,
    /// Held jobs released back to `queued` (restore only).
    pub jobs_released: u64,
}

/// The body for admin actions that need a reason (suspend,
/// impersonate): the reason is required — an admin action
/// without one is not auditable. Shared by every such action
/// so the shape (and the validation) has one definition.
#[derive(Debug, Deserialize, ToSchema)]
pub struct ReasonRequest {
    /// Why the action is taken (1–500 characters after
    /// trimming; the audit carries it verbatim).
    pub reason: String,
}

impl Validate for ReasonRequest {
    fn validate(&self) -> Result<(), ValidationErrors> {
        let mut errors = ValidationErrors::default();
        if let Err(err) = validate_reason("reason", &self.reason) {
            errors.push(err);
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

/// Suspend a user (P1-008). One transaction: the status flip
/// (+ `deleted_at` cleared — the grace is cancelled) + the
/// session revocations + the job holds + the audit. The audit's
/// `after` carries the counts, so the row is self-describing.
pub async fn suspend_user(
    state: &AppState,
    admin: &User,
    target_id: Uuid,
    req: &ReasonRequest,
) -> Result<AdminActionOutcome, AdminError> {
    // Validate (again — the handler's ValidatedJson is the
    // gate; this call is the transform that yields the trimmed
    // reason to store).
    let reason = validate_reason("reason", &req.reason)
        .map_err(|err| AdminError::Validation(ValidationErrors(vec![err])))?;
    let target = users::by_id(&state.pool, target_id).await.map_err(|err| {
        tracing::error!(error = ?err, "admin suspend: user lookup failed");
        AdminError::Internal
    })?;
    let Some(target) = target else {
        return Err(AdminError::NotFound);
    };
    if target.status == "suspended" {
        return Ok(AdminActionOutcome {
            user: target,
            already: true,
            sessions_revoked: 0,
            jobs_held: 0,
            jobs_released: 0,
        });
    }
    let timeout = PoolConfig::default().transaction_timeout;
    let (user, sessions_revoked, jobs_held) = tx::with_tx(&state.pool, timeout, |conn| {
        let target = target.clone();
        let admin = admin.clone();
        let reason = reason.clone();
        Box::pin(async move {
            let user = users::suspend(&mut *conn, target.id)
                .await
                .map_err(AdminTxError::Db)?;
            let sessions_revoked = sessions::revoke_all_for_user(&mut *conn, target.id)
                .await
                .map_err(AdminTxError::Db)?;
            let jobs_held = jobs::hold_queued_for_user(&mut *conn, target.id)
                .await
                .map_err(AdminTxError::Db)?;
            audit_logs::insert(
                &mut *conn,
                &NewAuditLog {
                    actor_user_id: Some(admin.id),
                    actor_client: None,
                    action: "user.suspended".to_string(),
                    target_type: "user".to_string(),
                    target_id: Some(target.id.to_string()),
                    before: Some(serde_json::json!({
                        "status": target.status,
                        "deleted_at": target.deleted_at,
                    })),
                    after: Some(serde_json::json!({
                        "status": "suspended",
                        "sessions_revoked": sessions_revoked,
                        "jobs_held": jobs_held,
                    })),
                    reason: Some(reason),
                },
            )
            .await
            .map_err(AdminTxError::Db)?;
            Ok((user, sessions_revoked, jobs_held))
        })
    })
    .await
    .map_err(|err| match err {
        TxError::Inner(AdminTxError::Db(RepoError::NotFound)) => AdminError::NotFound,
        TxError::Inner(AdminTxError::Db(db_err)) => {
            tracing::error!(error = ?db_err, "admin suspend: transaction failed (db)");
            AdminError::Internal
        }
        other => {
            tracing::error!(error = ?other, "admin suspend: transaction failed");
            AdminError::Internal
        }
    })?;
    Ok(AdminActionOutcome {
        user,
        already: false,
        sessions_revoked,
        jobs_held,
        jobs_released: 0,
    })
}

/// Restore a suspended user (P1-008). One transaction: the
/// status flip (+ `deleted_at` cleared — a suspension cancels
/// any pending deletion) + the held jobs released + the audit.
/// Sessions stay revoked (the user logs in again). Restoring a
/// non-suspended account is a no-op.
pub async fn restore_user(
    state: &AppState,
    admin: &User,
    target_id: Uuid,
) -> Result<AdminActionOutcome, AdminError> {
    let target = users::by_id(&state.pool, target_id).await.map_err(|err| {
        tracing::error!(error = ?err, "admin restore: user lookup failed");
        AdminError::Internal
    })?;
    let Some(target) = target else {
        return Err(AdminError::NotFound);
    };
    if target.status != "suspended" {
        return Ok(AdminActionOutcome {
            user: target,
            already: true,
            sessions_revoked: 0,
            jobs_held: 0,
            jobs_released: 0,
        });
    }
    let timeout = PoolConfig::default().transaction_timeout;
    let (user, jobs_released) = tx::with_tx(&state.pool, timeout, |conn| {
        let target = target.clone();
        let admin = admin.clone();
        Box::pin(async move {
            let user = users::restore(&mut *conn, target.id)
                .await
                .map_err(AdminTxError::Db)?;
            let jobs_released = jobs::release_held_for_user(&mut *conn, target.id)
                .await
                .map_err(AdminTxError::Db)?;
            audit_logs::insert(
                &mut *conn,
                &NewAuditLog {
                    actor_user_id: Some(admin.id),
                    actor_client: None,
                    action: "user.restored".to_string(),
                    target_type: "user".to_string(),
                    target_id: Some(target.id.to_string()),
                    before: Some(serde_json::json!({ "status": target.status })),
                    after: Some(serde_json::json!({
                        "status": "active",
                        "jobs_released": jobs_released,
                    })),
                    reason: None,
                },
            )
            .await
            .map_err(AdminTxError::Db)?;
            Ok((user, jobs_released))
        })
    })
    .await
    .map_err(|err| match err {
        TxError::Inner(AdminTxError::Db(RepoError::NotFound)) => AdminError::NotFound,
        TxError::Inner(AdminTxError::Db(db_err)) => {
            tracing::error!(error = ?db_err, "admin restore: transaction failed (db)");
            AdminError::Internal
        }
        other => {
            tracing::error!(error = ?other, "admin restore: transaction failed");
            AdminError::Internal
        }
    })?;
    Ok(AdminActionOutcome {
        user,
        already: false,
        sessions_revoked: 0,
        jobs_held: 0,
        jobs_released,
    })
}

/// One row of the admin user search (P1-009): the user as the
/// dashboard shows them — identity, status, plan, and today's
/// usage. `plan` is the plan code (`free` when the user has no
/// live subscription); `usage_today` is the UTC day's counters
/// (zeros when the user has used nothing today).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct UserSearchItem {
    pub id: Uuid,
    pub email: String,
    pub display_name: Option<String>,
    pub role: String,
    pub status: String,
    pub email_verified_at: Option<DateTime<Utc>>,
    pub tos_version: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub plan: String,
    pub images_used_today: i32,
    pub video_seconds_used_today: i32,
}

impl From<UserSearchRow> for UserSearchItem {
    fn from(row: UserSearchRow) -> Self {
        Self {
            id: row.id,
            email: row.email,
            display_name: row.display_name,
            role: row.role,
            status: row.status,
            email_verified_at: row.email_verified_at,
            tos_version: row.tos_version,
            created_at: row.created_at,
            updated_at: row.updated_at,
            // The query COALESCEs these (see the row doc) — a
            // `None` here would mean the COALESCE lied, so the
            // defaults are the documented fallbacks, not guesses.
            plan: row.plan_code.unwrap_or_else(|| "free".to_string()),
            images_used_today: row.images_used_today.unwrap_or(0),
            video_seconds_used_today: row.video_seconds_used_today.unwrap_or(0),
        }
    }
}

/// The search response page: the api-spec pagination shape
/// (`{"items": [...], "next_cursor": "..."}` — `next_cursor`
/// omitted on the last page). Built from core's `Page` (the
/// keyset math lives there); this struct exists so the
/// response has an OpenAPI schema (`Page<T>` is generic and
/// carries no `ToSchema`).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct UserSearchPage {
    pub items: Vec<UserSearchItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// Search users (P1-009). `query` is a prefix filter on
/// email, display name, or id (case-insensitive; blank is
/// no filter); `limit` clamps to 1..=100 (default 20);
/// `cursor` is the opaque keyset from the previous page — a
/// tampered cursor is 422 `PF-2002`, never a silent restart.
/// One query, no N+1: the plan code and today's usage come
/// back in the same rows (see `users::search_users`).
pub async fn search_users(
    state: &AppState,
    query: Option<String>,
    limit: Option<u32>,
    cursor: Option<String>,
) -> Result<UserSearchPage, AdminError> {
    let limit = clamp_limit(limit);
    let cursor = match cursor {
        Some(raw) => Some(Cursor::decode(&raw).map_err(|_| AdminError::InvalidCursor)?),
        None => None,
    };
    let query = query.as_deref().map(str::trim).filter(|q| !q.is_empty());
    let day = Utc::now().date_naive();
    let rows = users::search_users(&state.pool, query, cursor, limit + 1, day)
        .await
        .map_err(|err| {
            tracing::error!(error = ?err, "admin search: query failed");
            AdminError::Internal
        })?;
    let page = Page::from_window(rows, limit, |row| Cursor {
        // `timestamp_millis()` truncates to milliseconds — the
        // same truncation the repo's keyset compares on, so the
        // cursor points at exactly this row.
        ts: row.created_at.timestamp_millis(),
        id: row.id,
    });
    Ok(UserSearchPage {
        items: page.items.into_iter().map(UserSearchItem::from).collect(),
        next_cursor: page.next_cursor,
    })
}

/// The impersonation response (P1-010): a 15-minute scoped
/// token that acts as the target user, plus the two ids the
/// console needs for the banner — who is acting, and as whom.
/// There is no refresh token and no session row behind it:
/// the token dies at `exp` and cannot be renewed.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ImpersonateResponse {
    /// The HS256 access token (15 min), carrying
    /// `impersonator_id` in its claims.
    pub access_token: String,
    /// Always `"Bearer"`.
    pub token_type: String,
    /// Seconds until the access token expires (900).
    pub expires_in: i64,
    /// The admin who is impersonating (the token's
    /// `impersonator_id` claim).
    pub impersonator_id: Uuid,
    /// The user being impersonated (the token's `sub`).
    pub user_id: Uuid,
}

/// Impersonate a user (P1-010). Mints a 15-minute scoped
/// token that acts as the target (`sub` + `role` are the
/// target's) and carries the admin's id as
/// `impersonator_id`. Then audits `user.impersonated`
/// (actor = the admin, target = the user, the reason in the
/// audit's `reason`, `after` carries `{expires_in: 900}`).
///
/// The token is issued BEFORE the audit insert: the audit
/// records a token that was actually handed out. If the
/// audit fails, the client gets a 500 and never learns the
/// token string — an unusable token that exists only in
/// memory. There is no transaction spanning the two, because
/// token issuance is not a database operation.
///
/// What impersonation does NOT do: it does not create a
/// session (no refresh, no cookie, no row — the token dies
/// at `exp`); it does not bypass the target's suspension for
/// login (that check is the login's, and this is not a
/// login); it does not grant admin rights — the admin guards
/// reject any context carrying `impersonator_id`, so the
/// token cannot reach admin endpoints even when the target
/// is an admin. Any user may be impersonated, including
/// another admin — the audit is the control, not a
/// restriction the spec does not name.
pub async fn impersonate_user(
    state: &AppState,
    admin: &User,
    target_id: Uuid,
    req: &ReasonRequest,
) -> Result<ImpersonateResponse, AdminError> {
    // Validate (again — the handler's ValidatedJson is the
    // gate; this call is the transform that yields the trimmed
    // reason to store).
    let reason = validate_reason("reason", &req.reason)
        .map_err(|err| AdminError::Validation(ValidationErrors(vec![err])))?;
    let target = users::by_id(&state.pool, target_id).await.map_err(|err| {
        tracing::error!(error = ?err, "admin impersonate: user lookup failed");
        AdminError::Internal
    })?;
    let Some(target) = target else {
        return Err(AdminError::NotFound);
    };
    let access_token = detag_core::jwt::issue_impersonation_token(
        &state.settings.jwt_secret,
        target.id,
        &target.role,
        admin.id,
        detag_core::jwt::ACCESS_TOKEN_TTL_SECS,
    )
    .map_err(|err| {
        tracing::error!(error = ?err, "admin impersonate: token issue failed");
        AdminError::Internal
    })?;
    audit_logs::insert(
        &state.pool,
        &NewAuditLog {
            actor_user_id: Some(admin.id),
            actor_client: None,
            action: "user.impersonated".to_string(),
            target_type: "user".to_string(),
            target_id: Some(target.id.to_string()),
            before: None,
            after: Some(serde_json::json!({
                "expires_in": detag_core::jwt::ACCESS_TOKEN_TTL_SECS,
            })),
            reason: Some(reason),
        },
    )
    .await
    .map_err(|err| {
        tracing::error!(error = ?err, "admin impersonate: audit failed");
        AdminError::Internal
    })?;
    Ok(ImpersonateResponse {
        access_token,
        token_type: "Bearer".to_string(),
        expires_in: detag_core::jwt::ACCESS_TOKEN_TTL_SECS,
        impersonator_id: admin.id,
        user_id: target.id,
    })
}
