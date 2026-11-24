//! Admin routes (P0-064): `/v1/admin/*` (+ the public abuse
//! intake `/v1/abuse`).
//!
//! The abuse intake is public (rate-limited) and its tickets
//! are reviewed in the admin abuse queue, so it lives here.
//! The resolve endpoint is `POST
//! /v1/admin/abuse-reports/{id}/resolve` — the spec says only
//! "POST resolve with action" under the abuse-reports row.
//! All handlers are the shared 501 stub until their Phase 4/5
//! tickets land — except `POST /v1/admin/users/{id}/suspend`
//! and `/restore`, real since P1-008 (in this file), `GET
//! /v1/admin/users`, real since P1-009, `POST
//! /v1/admin/users/{id}/impersonate`, real since P1-010,
//! `GET /v1/admin/jobs`, real since P1-038 (in this file),
//! `GET` / `PUT /v1/admin/plans`, real since P1-056 (in
//! this file), and `POST /v1/admin/billing/catalog-sync`,
//! real since P1-061 (in this file), and `POST
//! /v1/admin/billing/subscriptions/{id}/cancel`, real since
//! P1-071 (in this file), and `GET
//! /v1/admin/usage/{id}` + `GET
//! /v1/admin/usage/{id}/export`, real since P1-081 (in
//! this file), and `GET /v1/admin/audit`, real since
//! P1-088 (in this file), and `GET
//! /v1/admin/abuse-reports` + `POST
//! /v1/admin/abuse-reports/{id}/resolve`, real since
//! P1-090 (in this file), and `POST /v1/abuse`, real since
//! P1-089 (in this file — the public intake, no guard),
//! and `POST /v1/admin/users/{id}/warn` +
//! `/v1/admin/users/{id}/terminate`, real since P1-091
//! (in this file), and `GET /v1/admin/users/{id}`, real
//! since P1-106 (in this file), and `POST
//! /v1/admin/users/{id}/quota-grant`, real since P1-107
//! (in this file), and `POST
//! /v1/admin/billing/subscriptions/{id}/refund`, real since
//! P1-108 (in this file).
//! Every route — live or stub — sits behind the matrix guard
//! (P1-043): the stubs take `admin_stub` / `admin_read_stub`
//! below, so an unauthenticated or wrong-role caller is
//! rejected before the 501, exactly like on the live routes.
//! The public abuse intake is live since P1-089 — it is not
//! an admin route, so it takes no guard and no credential.
//!
//! The live handlers take [`AdminContext`] (writes) or
//! [`AdminReadContext`] (reads): the extractor verifies the
//! credential and enforces the route's matrix row (P1-042,
//! FR-A-07) before the handler body runs, so a non-admitted
//! caller never reaches the service — 401 `PF-1001` without
//! a credential, 403 `PF-1002` for a verified wrong role, an
//! API key, or an impersonator. The handlers are thin: the admin
//! service owns the flow; this maps its errors to the
//! problem+json envelope. They return `Response` directly
//! (not `Result<_, ProblemResponse>` — the P1-001
//! `result_large_err` shape; see the adding-an-endpoint doc).

use super::extractors::{MaybeConnectInfo, RequestId, ValidatedJson};
use crate::{
    audit_middleware::{audit_admin_writes, AuditDetail, AuditHandled},
    auth::{AdminContext, AdminReadContext},
    problem::ProblemResponse,
    rate_limit::{client_ip_from, denied_response},
    services::{
        abuse_intake::{
            file_abuse_report, AbuseIntakeError, AbuseIntakeOutcome, AbuseIntakeRequest,
            AbuseReportResponse,
        },
        admin::{
            impersonate_user, list_jobs, restore_user, search_users, suspend_user, AdminError,
            AdminJobsPage, ImpersonateResponse, ReasonRequest, UserSearchPage,
        },
        admin_abuse_queue::{
            list_abuse_queue, resolve_abuse_report, AbuseQueueError, AbuseQueuePage,
            AbuseReportItem, ResolveAbuseReportRequest,
        },
        admin_attestations::{list_attestations, AttestationReviewError, AttestationsPage},
        admin_audit::{list_audit, AdminAuditError, AuditPage},
        admin_plans::{
            list_plans, update_plans, PlanEditorError, PlanView, PlansResponse, UpdatePlansRequest,
        },
        admin_usage::{export_usage_csv, get_admin_usage, AdminUsageError, AdminUsageResponse},
        admin_user_view::{get_admin_user_view, AdminUserViewError, AdminUserViewResponse},
        catalog_sync::{run_catalog_sync, CatalogSyncError},
        enforcement::{terminate_user, warn_user, EnforcementError, EnforcementResponse},
        quota_grant::{grant_quota, QuotaGrantError, QuotaGrantRequest, QuotaGrantResponse},
        refund_tool::{request_refund, RefundChecklistResponse, RefundToolError},
        register::UserResponse,
        support_cancel::{
            support_cancel as support_cancel_service, SupportCancelError, SupportCancelResponse,
        },
    },
    AppState,
};
use axum::{
    extract::{Extension, Path, Query, State},
    http::{HeaderMap, StatusCode},
    middleware,
    response::{IntoResponse, Json, Response},
    routing::{get, post},
    Router,
};
use chrono::{DateTime, Utc};
use detag_billing::CatalogDriftReport;
use detag_core::error::{AppError, Problem, Profile};
use detag_core::repos::audit_logs::AuditFilters;
use serde::Deserialize;
use uuid::Uuid;

/// Query params for `GET /v1/admin/users` (P1-009): all
/// optional. `query` is a prefix filter (email, display name,
/// or id — blank means no filter); `limit` clamps to 1..=100
/// (default 20); `cursor` is the opaque keyset from the
/// previous page.
#[derive(Debug, Deserialize)]
pub struct SearchUsersQuery {
    pub query: Option<String>,
    pub limit: Option<u32>,
    pub cursor: Option<String>,
}

/// GET /v1/admin/users (P1-009): search users. Admin +
/// support (the read-only role) may call it; a verified plain
/// user gets 403 `PF-1002` from the extractor, no credential
/// gets 401 `PF-1001`. Answers the api-spec pagination shape
/// (`{"items": [...], "next_cursor": "..."}` — `next_cursor`
/// omitted on the last page). A tampered cursor is 422
/// `PF-2002`; a non-numeric `limit` is rejected by the `Query`
/// extractor the same way (the error mapper rewrites the
/// framework 400 to the 422 problem).
#[utoipa::path(
    get,
    path = "/v1/admin/users",
    params(
        ("query" = Option<String>, Query, description = "Prefix filter on email, display name, or id (case-insensitive); blank lists everything"),
        ("limit" = Option<u32>, Query, description = "Page size, clamped to 1..=100 (default 20)"),
        ("cursor" = Option<String>, Query, description = "Opaque keyset from the previous page's next_cursor")
    ),
    responses(
        (status = 200, description = "One page of users", body = UserSearchPage),
        (status = 401, description = "Unauthenticated (no credential, bad or expired token)", body = Problem),
        (status = 403, description = "Forbidden (verified plain user)", body = Problem),
        (status = 422, description = "Tampered cursor or bad limit", body = Problem)
    ),
    tag = "admin",
    security(("bearer_auth" = []))
)]
pub async fn search_users_handler(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    _admin: AdminReadContext,
    Query(params): Query<SearchUsersQuery>,
) -> Response {
    let request_id = Some(id.0.as_str());
    match search_users(&state, params.query, params.limit, params.cursor).await {
        Ok(page) => Json(page).into_response(),
        Err(AdminError::InvalidCursor) => ProblemResponse(AppError::ValidationFailed.to_problem(
            Profile::Production,
            request_id,
            None,
        ))
        .into_response(),
        Err(AdminError::Validation(_)) | Err(AdminError::NotFound) => {
            // Search takes no validated body and never 404s.
            ProblemResponse(AppError::Internal.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
        Err(AdminError::Internal) => {
            ProblemResponse(AppError::Internal.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
    }
}

/// GET /v1/admin/users/{id} (P1-106): the support user
/// view — profile, plan, usage, recent jobs, session meta
/// for one user. Admin + support (the read-only role) may
/// call it; a verified plain user gets 403 `PF-1002` from
/// the extractor, no credential gets 401 `PF-1001`, an API
/// key gets 403. A user who does not exist is 404 `PF-2010`;
/// a user who exists but has no subscription is Free, no
/// jobs or no live sessions is empty arrays, not a 404. No
/// secrets anywhere in the shape (ADR-0065): no password
/// hash, no refresh hash, no provider ids. No audit row (a
/// read creates nothing).
#[utoipa::path(
    get,
    path = "/v1/admin/users/{id}",
    params(
        ("id" = Uuid, Path, description = "Whose view to read")
    ),
    responses(
        (status = 200, description = "The user's support view: profile, plan, usage, recent jobs, and live sessions", body = AdminUserViewResponse),
        (status = 401, description = "Unauthenticated (no credential, bad or expired token)", body = Problem),
        (status = 403, description = "Forbidden (verified plain user, or an API key)", body = Problem),
        (status = 404, description = "No such user", body = Problem),
        (status = 500, description = "The user view read failed", body = Problem)
    ),
    tag = "admin",
    security(("bearer_auth" = []))
)]
pub async fn admin_user_view_handler(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    _admin: AdminReadContext,
    Path(user_id): Path<Uuid>,
) -> Response {
    let request_id = Some(id.0.as_str());
    match get_admin_user_view(&state, user_id).await {
        Ok(view) => Json(view).into_response(),
        Err(AdminUserViewError::NotFound) => {
            ProblemResponse(AppError::NotFound.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
        Err(err) => {
            if let AdminUserViewError::Db(db_err) = &err {
                tracing::error!(error = %db_err, "admin user view: read failed");
            }
            ProblemResponse(AppError::Internal.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
    }
}

/// Query params for `GET /v1/admin/jobs` (P1-038): all
/// optional. `user_id` / `key_id` narrow the list to one
/// owner's jobs / one key's jobs (both absent lists
/// everything); `limit` clamps to 1..=100 (default 20);
/// `cursor` is the opaque keyset from the previous page.
#[derive(Debug, Deserialize)]
pub struct ListJobsQuery {
    pub user_id: Option<Uuid>,
    pub key_id: Option<Uuid>,
    pub limit: Option<u32>,
    pub cursor: Option<String>,
}

/// GET /v1/admin/jobs (P1-038): browse jobs, newest first.
/// Admin + support (the read-only role) may call it; a
/// verified plain user gets 403 `PF-1002` from the extractor,
/// no credential gets 401 `PF-1001`. Each row shows the
/// display prefix of the API key that created the job
/// (`key_prefix` is null for a web / JWT-created job) — the
/// prefix is the only key material this view ever shows.
/// Answers the api-spec pagination shape (`{"items": [...],
/// "next_cursor": "..."}` — `next_cursor` omitted on the
/// last page). A tampered cursor is 422 `PF-2002`; a
/// non-numeric `limit` or a malformed uuid filter is rejected
/// by the `Query` extractor the same way (the error mapper
/// rewrites the framework 400 to the 422 problem).
#[utoipa::path(
    get,
    path = "/v1/admin/jobs",
    params(
        ("user_id" = Option<Uuid>, Query, description = "Only this owner's jobs"),
        ("key_id" = Option<Uuid>, Query, description = "Only this API key's jobs (key-scoped attribution)"),
        ("limit" = Option<u32>, Query, description = "Page size, clamped to 1..=100 (default 20)"),
        ("cursor" = Option<String>, Query, description = "Opaque keyset from the previous page's next_cursor")
    ),
    responses(
        (status = 200, description = "One page of jobs with the creating key's prefix", body = AdminJobsPage),
        (status = 401, description = "Unauthenticated (no credential, bad or expired token)", body = Problem),
        (status = 403, description = "Forbidden (verified plain user)", body = Problem),
        (status = 422, description = "Tampered cursor, bad limit, or malformed uuid filter", body = Problem)
    ),
    tag = "admin",
    security(("bearer_auth" = []))
)]
pub async fn list_jobs_handler(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    _admin: AdminReadContext,
    Query(params): Query<ListJobsQuery>,
) -> Response {
    let request_id = Some(id.0.as_str());
    match list_jobs(
        &state,
        params.user_id,
        params.key_id,
        params.limit,
        params.cursor,
    )
    .await
    {
        Ok(page) => Json(page).into_response(),
        Err(AdminError::InvalidCursor) => ProblemResponse(AppError::ValidationFailed.to_problem(
            Profile::Production,
            request_id,
            None,
        ))
        .into_response(),
        Err(AdminError::Validation(_) | AdminError::NotFound) => {
            // Listing takes no validated body and never 404s.
            ProblemResponse(AppError::Internal.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
        Err(AdminError::Internal) => {
            ProblemResponse(AppError::Internal.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
    }
}

/// POST /v1/admin/users/{id}/suspend (P1-008): suspend the
/// account with a reason. Status becomes `suspended`, every
/// live session is revoked, every queued job is held, and the
/// action is audited (`user.suspended`, actor = the admin).
/// Idempotent: suspending an already-suspended account is a
/// 200 no-op (no write, no audit). A suspend cancels any
/// pending deletion (`deleted_at` cleared — the grace does not
/// survive admin intervention). Returns the updated user.
#[utoipa::path(
    post,
    path = "/v1/admin/users/{id}/suspend",
    request_body = ReasonRequest,
    responses(
        (status = 200, description = "Suspended (or already suspended)", body = UserResponse),
        (status = 401, description = "Unauthenticated (no credential, bad or expired token)", body = Problem),
        (status = 403, description = "Forbidden (verified non-admin)", body = Problem),
        (status = 404, description = "No such user", body = Problem),
        (status = 422, description = "Reason missing, blank, too long, or with control characters", body = Problem)
    ),
    tag = "admin",
    security(("bearer_auth" = []))
)]
pub async fn suspend(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    admin: AdminContext,
    Path(user_id): Path<Uuid>,
    ValidatedJson(req): ValidatedJson<ReasonRequest>,
) -> Response {
    let request_id = Some(id.0.as_str());
    let Some(admin_user) = admin.auth.user else {
        // The extractor guarantees a user for a verified context.
        return ProblemResponse(AppError::Internal.to_problem(
            Profile::Production,
            request_id,
            None,
        ))
        .into_response();
    };
    match suspend_user(&state, &admin_user, user_id, &req).await {
        Ok(outcome) => {
            // The service wrote the audit row in the same
            // transaction as the mutation (P1-008) — mark the
            // response so the P1-046 middleware stands down and
            // the row exists exactly once (also on the
            // idempotent no-op, which deliberately writes none).
            let mut response = Json(UserResponse::from(outcome.user)).into_response();
            response.extensions_mut().insert(AuditHandled);
            response
        }
        Err(AdminError::Validation(errors)) => {
            ProblemResponse(errors.into_problem(Profile::Production, request_id)).into_response()
        }
        Err(AdminError::NotFound) => {
            ProblemResponse(AppError::NotFound.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
        // Suspend takes no cursor — unreachable here.
        Err(AdminError::InvalidCursor) | Err(AdminError::Internal) => {
            ProblemResponse(AppError::Internal.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
    }
}

/// POST /v1/admin/users/{id}/warn (P1-091): the ladder's
/// first rung — a formal warning with a reason. The status
/// does not change; the warning is audited (`user.warned`,
/// actor = the admin) and the `enforcement-warning` mail
/// is enqueued in the same transaction. A warning is
/// repeatable: each call is a new warning event. Requires
/// an `active` account — warning a suspended or
/// deletion-pending one is 422 (the ladder only moves
/// forward). Returns the user + the rung that ran.
#[utoipa::path(
    post,
    path = "/v1/admin/users/{id}/warn",
    request_body = ReasonRequest,
    responses(
        (status = 200, description = "Warned", body = EnforcementResponse),
        (status = 401, description = "Unauthenticated (no credential, bad or expired token)", body = Problem),
        (status = 403, description = "Forbidden (verified non-admin)", body = Problem),
        (status = 404, description = "No such user", body = Problem),
        (status = 422, description = "Reason missing, blank, too long, or with control characters; or the account is not active", body = Problem)
    ),
    tag = "admin",
    security(("bearer_auth" = []))
)]
pub async fn warn_user_handler(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    admin: AdminContext,
    Path(user_id): Path<Uuid>,
    ValidatedJson(req): ValidatedJson<ReasonRequest>,
) -> Response {
    let request_id = Some(id.0.as_str());
    let Some(admin_user) = admin.auth.user else {
        // The extractor guarantees a user for a verified context.
        return ProblemResponse(AppError::Internal.to_problem(
            Profile::Production,
            request_id,
            None,
        ))
        .into_response();
    };
    match warn_user(&state, &admin_user, user_id, &req).await {
        Ok(outcome) => {
            // The service wrote the audit row in the same
            // transaction as the mail (P1-091) — mark the
            // response so the P1-046 middleware stands down
            // and the row exists exactly once.
            let mut response = Json(EnforcementResponse::from(outcome)).into_response();
            response.extensions_mut().insert(AuditHandled);
            response
        }
        Err(EnforcementError::Validation(errors)) => {
            ProblemResponse(errors.into_problem(Profile::Production, request_id)).into_response()
        }
        Err(EnforcementError::NotFound) => {
            ProblemResponse(AppError::NotFound.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
        Err(EnforcementError::InvalidStatus) => ProblemResponse(
            AppError::ValidationFailed.to_problem(Profile::Production, request_id, None),
        )
        .into_response(),
        Err(EnforcementError::Internal) => {
            ProblemResponse(AppError::Internal.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
    }
}

/// POST /v1/admin/users/{id}/quota-grant (P1-107): a manual,
/// expiring quota bump — extra daily images and/or
/// video-seconds for `ttl_hours` (1–168), with a reason. The
/// grant and its `admin.users.quota_granted` audit row commit
/// in one transaction, and the response carries
/// `AuditHandled` so the P1-046 middleware stands down and
/// the row exists exactly once. The bump is honored by the
/// quota snapshot (the pre-check, the profile, and the quota
/// warnings all read the same limits), and it stops counting
/// the moment `expires_at` passes. Returns the created grant
/// with 200 — an admin action on a user answers 200 (the
/// suspend/warn precedent; the authz live walk pins 200 for
/// every live admin write except the catalog sync), not 201,
/// even though a row is created. Admin-only — support is
/// read-only.
#[utoipa::path(
    post,
    path = "/v1/admin/users/{id}/quota-grant",
    request_body = QuotaGrantRequest,
    responses(
        (status = 200, description = "Granted", body = QuotaGrantResponse),
        (status = 401, description = "Unauthenticated (no credential, bad or expired token)", body = Problem),
        (status = 403, description = "Forbidden (verified non-admin, or an API key)", body = Problem),
        (status = 404, description = "No such user", body = Problem),
        (status = 422, description = "No amount, a non-positive amount, ttl_hours outside 1..=168, or a bad reason", body = Problem)
    ),
    tag = "admin",
    security(("bearer_auth" = []))
)]
pub async fn quota_grant_handler(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    admin: AdminContext,
    Path(user_id): Path<Uuid>,
    ValidatedJson(req): ValidatedJson<QuotaGrantRequest>,
) -> Response {
    let request_id = Some(id.0.as_str());
    let Some(admin_user) = admin.auth.user else {
        // The extractor guarantees a user for a verified context.
        return ProblemResponse(AppError::Internal.to_problem(
            Profile::Production,
            request_id,
            None,
        ))
        .into_response();
    };
    match grant_quota(&state, &admin_user, user_id, &req).await {
        Ok(grant) => {
            // The service wrote the audit row in the same
            // transaction as the grant (P1-107) — mark the
            // response so the P1-046 middleware stands down
            // and the row exists exactly once.
            let mut response = Json(QuotaGrantResponse::from(grant)).into_response();
            response.extensions_mut().insert(AuditHandled);
            response
        }
        Err(QuotaGrantError::Validation(errors)) => {
            ProblemResponse(errors.into_problem(Profile::Production, request_id)).into_response()
        }
        Err(QuotaGrantError::NotFound) => {
            ProblemResponse(AppError::NotFound.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
        Err(QuotaGrantError::Internal) => {
            ProblemResponse(AppError::Internal.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
    }
}

/// POST /v1/admin/users/{id}/terminate (P1-091): the
/// ladder's third rung — termination with a reason. Status
/// becomes `deletion_pending` (the P1-007 grace: purge
/// after 30 days), every live session is revoked, every
/// queued job is held, and the action is audited
/// (`user.terminated`, actor = the admin) with the
/// `enforcement-termination` mail enqueued in the same
/// transaction. Requires a `suspended` account — the ladder
/// is sequential, an active account cannot be terminated
/// outright (422). Idempotent: terminating an
/// already-terminated account is a 200 no-op (no write, no
/// audit, no mail). A terminated account cannot log in
/// (403 `PF-1004`) — the login does NOT cancel an
/// admin-initiated termination the way it cancels a
/// self-requested deletion. Returns the user + the rung
/// that ran.
#[utoipa::path(
    post,
    path = "/v1/admin/users/{id}/terminate",
    request_body = ReasonRequest,
    responses(
        (status = 200, description = "Terminated (or already terminated)", body = EnforcementResponse),
        (status = 401, description = "Unauthenticated (no credential, bad or expired token)", body = Problem),
        (status = 403, description = "Forbidden (verified non-admin)", body = Problem),
        (status = 404, description = "No such user", body = Problem),
        (status = 422, description = "Reason missing, blank, too long, or with control characters; or the account is not suspended", body = Problem)
    ),
    tag = "admin",
    security(("bearer_auth" = []))
)]
pub async fn terminate_user_handler(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    admin: AdminContext,
    Path(user_id): Path<Uuid>,
    ValidatedJson(req): ValidatedJson<ReasonRequest>,
) -> Response {
    let request_id = Some(id.0.as_str());
    let Some(admin_user) = admin.auth.user else {
        // The extractor guarantees a user for a verified context.
        return ProblemResponse(AppError::Internal.to_problem(
            Profile::Production,
            request_id,
            None,
        ))
        .into_response();
    };
    match terminate_user(&state, &admin_user, user_id, &req).await {
        Ok(outcome) => {
            // Same marker protocol as warn (P1-046): the
            // service wrote the audit row in the mutation's
            // transaction (also on the idempotent no-op,
            // which deliberately writes none).
            let mut response = Json(EnforcementResponse::from(outcome)).into_response();
            response.extensions_mut().insert(AuditHandled);
            response
        }
        Err(EnforcementError::Validation(errors)) => {
            ProblemResponse(errors.into_problem(Profile::Production, request_id)).into_response()
        }
        Err(EnforcementError::NotFound) => {
            ProblemResponse(AppError::NotFound.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
        Err(EnforcementError::InvalidStatus) => ProblemResponse(
            AppError::ValidationFailed.to_problem(Profile::Production, request_id, None),
        )
        .into_response(),
        Err(EnforcementError::Internal) => {
            ProblemResponse(AppError::Internal.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
    }
}

/// POST /v1/admin/users/{id}/restore (P1-008): restore a
/// suspended account. Status becomes `active`, held jobs are
/// released back to `queued`, and the action is audited
/// (`user.restored`, actor = the admin). Sessions stay revoked
/// (the user logs in again). Idempotent: restoring a
/// non-suspended account is a 200 no-op. Returns the updated
/// user.
#[utoipa::path(
    post,
    path = "/v1/admin/users/{id}/restore",
    responses(
        (status = 200, description = "Restored (or not suspended)", body = UserResponse),
        (status = 401, description = "Unauthenticated (no credential, bad or expired token)", body = Problem),
        (status = 403, description = "Forbidden (verified non-admin)", body = Problem),
        (status = 404, description = "No such user", body = Problem)
    ),
    tag = "admin",
    security(("bearer_auth" = []))
)]
pub async fn restore(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    admin: AdminContext,
    Path(user_id): Path<Uuid>,
) -> Response {
    let request_id = Some(id.0.as_str());
    let Some(admin_user) = admin.auth.user else {
        // The extractor guarantees a user for a verified context.
        return ProblemResponse(AppError::Internal.to_problem(
            Profile::Production,
            request_id,
            None,
        ))
        .into_response();
    };
    match restore_user(&state, &admin_user, user_id).await {
        Ok(outcome) => {
            // Same marker protocol as suspend (P1-046): the
            // service owns the row, the middleware stands down.
            let mut response = Json(UserResponse::from(outcome.user)).into_response();
            response.extensions_mut().insert(AuditHandled);
            response
        }
        // Restore takes no body and no cursor, so Validation
        // and InvalidCursor are unreachable here — they share
        // the enum with suspend and search.
        Err(AdminError::Validation(_) | AdminError::InvalidCursor | AdminError::Internal) => {
            ProblemResponse(AppError::Internal.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
        Err(AdminError::NotFound) => {
            ProblemResponse(AppError::NotFound.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
    }
}

/// POST /v1/admin/users/{id}/impersonate (P1-010): mint a
/// 15-minute scoped token that acts as the target user. Admin
/// only (support may not impersonate — it is a write, and the
/// `AdminContext` extractor rejects support with 403
/// `PF-1002`). The token carries `impersonator_id` in its
/// claims; it has no refresh and no session row, so it dies
/// at `exp`. Returns the token + the two ids the console
/// needs for the banner.
#[utoipa::path(
    post,
    path = "/v1/admin/users/{id}/impersonate",
    request_body = ReasonRequest,
    responses(
        (status = 200, description = "Scoped impersonation token (15 min)", body = ImpersonateResponse),
        (status = 401, description = "Unauthenticated (no credential, bad or expired token)", body = Problem),
        (status = 403, description = "Forbidden (verified non-admin, incl. support)", body = Problem),
        (status = 404, description = "No such user", body = Problem),
        (status = 422, description = "Reason missing, blank, too long, or with control characters", body = Problem)
    ),
    tag = "admin",
    security(("bearer_auth" = []))
)]
pub async fn impersonate(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    admin: AdminContext,
    Path(user_id): Path<Uuid>,
    ValidatedJson(req): ValidatedJson<ReasonRequest>,
) -> Response {
    let request_id = Some(id.0.as_str());
    let Some(admin_user) = admin.auth.user else {
        // The extractor guarantees a user for a verified context.
        return ProblemResponse(AppError::Internal.to_problem(
            Profile::Production,
            request_id,
            None,
        ))
        .into_response();
    };
    match impersonate_user(&state, &admin_user, user_id, &req).await {
        Ok(response) => {
            // Same marker protocol as suspend (P1-046): the
            // service owns the row, the middleware stands down.
            let mut response = Json(response).into_response();
            response.extensions_mut().insert(AuditHandled);
            response
        }
        Err(AdminError::Validation(errors)) => {
            ProblemResponse(errors.into_problem(Profile::Production, request_id)).into_response()
        }
        Err(AdminError::NotFound) => {
            ProblemResponse(AppError::NotFound.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
        // Impersonation takes no cursor — unreachable here.
        Err(AdminError::InvalidCursor) | Err(AdminError::Internal) => {
            ProblemResponse(AppError::Internal.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
    }
}

/// GET /v1/admin/plans (P1-056): the plan catalog for the
/// editor — all plans, cheapest first, with the parsed
/// entitlements (the same seven keys the profile and the
/// quota middleware read). Admin + support (the read-only
/// role) may call it; a verified plain user gets 403
/// `PF-1002` from the extractor, no credential gets 401
/// `PF-1001`.
#[utoipa::path(
    get,
    path = "/v1/admin/plans",
    responses(
        (status = 200, description = "The plan catalog, cheapest first", body = PlansResponse),
        (status = 401, description = "Unauthenticated (no credential, bad or expired token)", body = Problem),
        (status = 403, description = "Forbidden (verified plain user)", body = Problem)
    ),
    tag = "admin",
    security(("bearer_auth" = []))
)]
pub async fn list_plans_handler(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    _admin: AdminReadContext,
) -> Response {
    let request_id = Some(id.0.as_str());
    match list_plans(&state).await {
        Ok(plans) => Json(PlansResponse {
            plans: plans.iter().map(PlanView::from).collect(),
        })
        .into_response(),
        Err(_) => {
            ProblemResponse(AppError::Internal.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
    }
}

/// PUT /v1/admin/plans (P1-056): batch-edit the catalog —
/// per-plan updates (`plans`, keyed by `code`) plus the
/// codes to delete (`remove`). Admin only; support gets 403
/// `PF-1002` from the extractor, no credential gets 401
/// `PF-1001`. A malformed or invalid edit is 422 `PF-2002`
/// (with `field_errors`); an unknown plan code is 404
/// `PF-2010`; removing a plan with subscriptions is 409
/// `PF-2012`. The whole PUT applies in one transaction, so
/// a blocked removal rolls the updates back too. On success
/// the catalog cache is invalidated before the response is
/// built, so the returned catalog is the fresh one; the
/// write is audited by the P1-046 middleware
/// (`admin.plans.updated`) with the changed codes attached
/// as the audit `after` — the handler does NOT mark
/// `AuditHandled`.
#[utoipa::path(
    put,
    path = "/v1/admin/plans",
    request_body = UpdatePlansRequest,
    responses(
        (status = 200, description = "The updated catalog, cheapest first", body = PlansResponse),
        (status = 401, description = "Unauthenticated (no credential, bad or expired token)", body = Problem),
        (status = 403, description = "Forbidden (verified non-admin, incl. support)", body = Problem),
        (status = 404, description = "No such plan code", body = Problem),
        (status = 409, description = "Plan has subscriptions and cannot be removed", body = Problem),
        (status = 422, description = "Invalid edit (entitlements, price, currency, Stripe price id, or the free-plan rules)", body = Problem)
    ),
    tag = "admin",
    security(("bearer_auth" = []))
)]
pub async fn update_plans_handler(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    _admin: AdminContext,
    ValidatedJson(req): ValidatedJson<UpdatePlansRequest>,
) -> Response {
    let request_id = Some(id.0.as_str());
    match update_plans(&state, &req).await {
        Ok(summary) => {
            // The edit committed — drop the cached catalog
            // so the next read (profile, quota pre-check,
            // feature gate) sees it at once.
            state.plans.invalidate();
            let plans = match list_plans(&state).await {
                Ok(plans) => plans,
                Err(_) => {
                    return ProblemResponse(AppError::Internal.to_problem(
                        Profile::Production,
                        request_id,
                        None,
                    ))
                    .into_response();
                }
            };
            let mut response = Json(PlansResponse {
                plans: plans.iter().map(PlanView::from).collect(),
            })
            .into_response();
            response.extensions_mut().insert(AuditDetail {
                before: None,
                after: Some(serde_json::json!({
                    "updated": summary.updated,
                    "removed": summary.removed,
                })),
                reason: None,
            });
            response
        }
        Err(PlanEditorError::Validation(errors)) => {
            ProblemResponse(errors.into_problem(Profile::Production, request_id)).into_response()
        }
        Err(PlanEditorError::NotFound) => {
            ProblemResponse(AppError::NotFound.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
        Err(PlanEditorError::HasSubscriptions) => ProblemResponse(
            AppError::PlanHasSubscriptions.to_problem(Profile::Production, request_id, None),
        )
        .into_response(),
        Err(PlanEditorError::Internal) => {
            ProblemResponse(AppError::Internal.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
    }
}

/// POST /v1/admin/billing/catalog-sync (P1-061): run the
/// products/prices catalog sync now and return the drift
/// report. Admin only; support gets 403 `PF-1002` from the
/// extractor, no credential gets 401 `PF-1001`, an API key
/// gets 403. No Stripe key configured is 503 `PF-5005`; a
/// provider failure mid-sync is 500 `PF-5000` (the provider
/// message never reaches the problem body). On success the
/// changed `stripe_price_id` values are already persisted and
/// the plan cache invalidated; the write is audited by the
/// P1-046 middleware (`admin.billing.catalog_synced`) with
/// the changed price ids attached as the audit `after` — the
/// handler does NOT mark `AuditHandled`.
#[utoipa::path(
    post,
    path = "/v1/admin/billing/catalog-sync",
    responses(
        (status = 200, description = "The drift report — one line per plan, cheapest first", body = CatalogDriftReport),
        (status = 401, description = "Unauthenticated (no credential, bad or expired token)", body = Problem),
        (status = 403, description = "Forbidden (verified non-admin, incl. support, or an API key)", body = Problem),
        (status = 500, description = "The provider call failed", body = Problem),
        (status = 503, description = "Billing is not configured (no Stripe secret key)", body = Problem)
    ),
    tag = "admin",
    security(("bearer_auth" = []))
)]
pub async fn catalog_sync_handler(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    _admin: AdminContext,
) -> Response {
    let request_id = Some(id.0.as_str());
    match run_catalog_sync(&state).await {
        Ok(report) => {
            let updates: Vec<serde_json::Value> = report
                .price_id_updates()
                .iter()
                .map(|(code, price_id)| serde_json::json!({"plan": code, "price_id": price_id}))
                .collect();
            let mut response = Json(report).into_response();
            response.extensions_mut().insert(AuditDetail {
                before: None,
                after: Some(serde_json::json!({"updated": updates})),
                reason: None,
            });
            response
        }
        Err(CatalogSyncError::NotConfigured) => ProblemResponse(
            AppError::BillingNotConfigured.to_problem(Profile::Production, request_id, None),
        )
        .into_response(),
        Err(CatalogSyncError::Provider | CatalogSyncError::Internal) => {
            ProblemResponse(AppError::Internal.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
    }
}

/// POST /v1/admin/billing/subscriptions/{id}/cancel
/// (P1-071): cancel a subscription immediately, as the
/// calling admin, for the support workflow. Admin only;
/// support gets 403 `PF-1002` from the extractor (support
/// stays read-only per FR-A-07 — "support-facing" describes
/// the workflow, not a new role), no credential gets 401
/// `PF-1001`, an API key gets 403. The body is the audited
/// note (`ReasonRequest`, 1–500 chars — the P1-008 suspend
/// precedent); it lands on the audit row verbatim. The row
/// moves to `canceled` NOW (not at period end, like a
/// P1-069 downgrade-to-Free); a pending scheduled change is
/// mooted; the `subscription-canceled` mail goes out; the
/// response carries the ToS policy snippet + the Stripe
/// dashboard refund guidance. No provider call is made — the
/// Stripe subscription is NOT canceled here and no refund is
/// issued (ASSUMPTION-034). Canceling an already-canceled
/// subscription is a 200 no-op (`changed: false`). The
/// service wrote the audit row in the same transaction as
/// the move, so the handler marks `AuditHandled` and the
/// P1-046 middleware stands down (also on the no-op, which
/// deliberately writes none) — the row exists exactly once.
#[utoipa::path(
    post,
    path = "/v1/admin/billing/subscriptions/{id}/cancel",
    request_body = ReasonRequest,
    responses(
        (status = 200, description = "Canceled (or already canceled) — the policy snippet and the dashboard refund guidance ride the response", body = SupportCancelResponse),
        (status = 401, description = "Unauthenticated (no credential, bad or expired token)", body = Problem),
        (status = 403, description = "Forbidden (verified non-admin, incl. support, or an API key)", body = Problem),
        (status = 404, description = "No such subscription (PF-2010)", body = Problem),
        (status = 409, description = "The move lost a race with a concurrent transition (PF-5002) — re-read and retry", body = Problem),
        (status = 422, description = "Reason missing, blank, too long, or with control characters", body = Problem)
    ),
    tag = "admin",
    security(("bearer_auth" = []))
)]
pub async fn cancel_subscription(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    admin: AdminContext,
    Path(subscription_id): Path<Uuid>,
    ValidatedJson(req): ValidatedJson<ReasonRequest>,
) -> Response {
    let request_id = Some(id.0.as_str());
    let Some(admin_user) = admin.auth.user else {
        // The extractor guarantees a user for a verified context.
        return ProblemResponse(AppError::Internal.to_problem(
            Profile::Production,
            request_id,
            None,
        ))
        .into_response();
    };
    match support_cancel_service(&state, &admin_user, subscription_id, &req.reason).await {
        Ok(outcome) => {
            // The service wrote the audit row in the same
            // transaction as the mutation (P1-064) — mark the
            // response so the P1-046 middleware stands down and
            // the row exists exactly once (also on the
            // idempotent no-op, which deliberately writes none).
            let mut response = Json(outcome).into_response();
            response.extensions_mut().insert(AuditHandled);
            response
        }
        Err(SupportCancelError::NotFound) => {
            ProblemResponse(AppError::NotFound.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
        Err(SupportCancelError::Conflict) => ProblemResponse(
            AppError::InvalidStateTransition.to_problem(Profile::Production, request_id, None),
        )
        .into_response(),
        Err(SupportCancelError::Internal) => {
            ProblemResponse(AppError::Internal.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
    }
}

/// POST /v1/admin/billing/subscriptions/{id}/refund
/// (P1-108): the guided checklist for refunding a
/// subscription in the Stripe dashboard, for the support
/// workflow. Admin only; support gets 403 `PF-1002` from
/// the extractor (support stays read-only per FR-A-07),
/// no credential gets 401 `PF-1001`, an API key gets 403.
/// The body is the audited note (`ReasonRequest`, 1–500
/// chars — the P1-008 suspend precedent); it lands on the
/// audit row verbatim. The response carries the Stripe
/// dashboard deep-link + note (reused `RefundGuidance`
/// from P1-071), a four-step checklist (all `done: false`
/// — the API cannot know what a human did in the
/// dashboard), and the required fields (`amount` and
/// `invoice_or_payment_id` are `null` — only support
/// knows them, and the tool makes no provider read).
/// One `billing.refund.requested` audit row per call; no
/// state change, no provider call, no mail, no money moved
/// (ASSUMPTION-034). Any status may be refunded, including
/// already-canceled. The service wrote the audit row, so
/// the handler marks `AuditHandled` and the P1-046
/// middleware stands down — the row exists exactly once.
#[utoipa::path(
    post,
    path = "/v1/admin/billing/subscriptions/{id}/refund",
    request_body = ReasonRequest,
    responses(
        (status = 200, description = "The guided checklist: the dashboard deep-link, the four steps, and the required fields", body = RefundChecklistResponse),
        (status = 401, description = "Unauthenticated (no credential, bad or expired token)", body = Problem),
        (status = 403, description = "Forbidden (verified non-admin, incl. support, or an API key)", body = Problem),
        (status = 404, description = "No such subscription (PF-2010)", body = Problem),
        (status = 422, description = "Reason missing, blank, too long, or with control characters", body = Problem)
    ),
    tag = "admin",
    security(("bearer_auth" = []))
)]
pub async fn refund_tool_handler(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    admin: AdminContext,
    Path(subscription_id): Path<Uuid>,
    ValidatedJson(req): ValidatedJson<ReasonRequest>,
) -> Response {
    let request_id = Some(id.0.as_str());
    let Some(admin_user) = admin.auth.user else {
        // The extractor guarantees a user for a verified context.
        return ProblemResponse(AppError::Internal.to_problem(
            Profile::Production,
            request_id,
            None,
        ))
        .into_response();
    };
    match request_refund(&state, &admin_user, subscription_id, &req.reason).await {
        Ok(outcome) => {
            // The service wrote the audit row — mark the
            // response so the P1-046 middleware stands down
            // and the row exists exactly once.
            let mut response = Json(outcome).into_response();
            response.extensions_mut().insert(AuditHandled);
            response
        }
        Err(RefundToolError::Validation(errors)) => {
            ProblemResponse(errors.into_problem(Profile::Production, request_id)).into_response()
        }
        Err(RefundToolError::NotFound) => {
            ProblemResponse(AppError::NotFound.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
        Err(RefundToolError::Internal) => {
            ProblemResponse(AppError::Internal.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
    }
}

/// GET /v1/admin/usage/{id} (P1-081): the admin usage
/// view — the P1-080 shape for an arbitrary user (today +
/// the 30-day window ending today from the durable
/// `usage_counters` rows) plus the all-time totals and
/// today's per-key breakdown (always present here — an
/// admin sees everything). Admin + support (the read-only
/// role) may call it; a verified plain user gets 403
/// `PF-1002` from the extractor, no credential gets 401
/// `PF-1001`, an API key gets 403. A user who does not
/// exist is 404 `PF-2010`; a user who exists but used
/// nothing gets zeros, not a 404. No audit row (a read
/// creates nothing).
#[utoipa::path(
    get,
    path = "/v1/admin/usage/{id}",
    params(
        ("id" = Uuid, Path, description = "Whose usage to view")
    ),
    responses(
        (status = 200, description = "The user's usage: today, the 30-day window, the all-time totals, and today's per-key breakdown", body = AdminUsageResponse),
        (status = 401, description = "Unauthenticated (no credential, bad or expired token)", body = Problem),
        (status = 403, description = "Forbidden (verified plain user, or an API key)", body = Problem),
        (status = 404, description = "No such user", body = Problem),
        (status = 500, description = "The usage read failed", body = Problem)
    ),
    tag = "admin",
    security(("bearer_auth" = []))
)]
pub async fn admin_usage_handler(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    _admin: AdminReadContext,
    Path(user_id): Path<Uuid>,
) -> Response {
    let request_id = Some(id.0.as_str());
    match get_admin_usage(&state, user_id).await {
        Ok(usage) => Json(usage).into_response(),
        Err(AdminUsageError::NotFound) => {
            ProblemResponse(AppError::NotFound.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
        Err(err) => {
            if let AdminUsageError::Db(db_err) = &err {
                tracing::error!(error = %db_err, "admin usage: read failed");
            }
            ProblemResponse(AppError::Internal.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
    }
}

/// GET /v1/admin/usage/{id}/export (P1-081): the same
/// 30-day window as CSV, for billing disputes — one header
/// row (`date,images,video_seconds`) plus one row per day,
/// oldest first. Admin + support may call it; a verified
/// plain user gets 403 `PF-1002` from the extractor, no
/// credential gets 401 `PF-1001`, an API key gets 403. A
/// user who does not exist is 404 `PF-2010`; a user who
/// exists but used nothing gets the header plus 30 zero
/// rows, not a 404. The response is `text/csv` with a
/// `Content-Disposition: attachment` filename, so a browser
/// downloads it instead of rendering it. No audit row (a
/// read creates nothing).
#[utoipa::path(
    get,
    path = "/v1/admin/usage/{id}/export",
    params(
        ("id" = Uuid, Path, description = "Whose usage to export")
    ),
    responses(
        (status = 200, description = "The user's 30-day usage as CSV: header date,images,video_seconds plus one row per day, oldest first", body = String, content_type = "text/csv"),
        (status = 401, description = "Unauthenticated (no credential, bad or expired token)", body = Problem),
        (status = 403, description = "Forbidden (verified plain user, or an API key)", body = Problem),
        (status = 404, description = "No such user", body = Problem),
        (status = 500, description = "The usage read failed", body = Problem)
    ),
    tag = "admin",
    security(("bearer_auth" = []))
)]
pub async fn admin_usage_export_handler(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    _admin: AdminReadContext,
    Path(user_id): Path<Uuid>,
) -> Response {
    let request_id = Some(id.0.as_str());
    match export_usage_csv(&state, user_id).await {
        Ok(csv) => axum::response::Response::builder()
            .status(StatusCode::OK)
            .header(axum::http::header::CONTENT_TYPE, "text/csv; charset=utf-8")
            .header(
                axum::http::header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"usage-{user_id}.csv\""),
            )
            .body(axum::body::Body::from(csv))
            .expect("the CSV response builds"),
        Err(AdminUsageError::NotFound) => {
            ProblemResponse(AppError::NotFound.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
        Err(err) => {
            if let AdminUsageError::Db(db_err) = &err {
                tracing::error!(error = %db_err, "admin usage export: read failed");
            }
            ProblemResponse(AppError::Internal.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
    }
}

/// Query params for `GET /v1/admin/audit` (P1-088): all
/// optional. `actor` is the acting user's id; `action` the
/// exact dotted action name; `target` the target's id (any
/// target type); `from`/`to` the half-open created-at
/// window `[from, to)` (RFC 3339); `limit` clamps to
/// 1..=100 (default 20); `cursor` is the opaque keyset
/// from the previous page.
#[derive(Debug, Deserialize)]
pub struct AuditQuery {
    pub actor: Option<Uuid>,
    pub action: Option<String>,
    pub target: Option<String>,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub limit: Option<u32>,
    pub cursor: Option<String>,
}

/// GET /v1/admin/audit (P1-088): one page of the audit
/// trail, newest first. Admin + support (the read-only
/// role) may call it; a verified plain user gets 403
/// `PF-1002` from the extractor, no credential gets 401
/// `PF-1001`, an API key gets 403. The filters combine
/// with AND (an unknown actor id is an empty page, not a
/// 404 — a list never 404s on its filter). Answers the
/// api-spec pagination shape (`{"items": [...],
/// "next_cursor": "..."}` — `next_cursor` omitted on the
/// last page). A tampered cursor is 422 `PF-2002`; a
/// malformed uuid filter or a malformed `from`/`to` is
/// rejected by the `Query` extractor the same way (the
/// error mapper rewrites the framework 400 to the 422
/// problem). No audit row (a read creates nothing).
#[utoipa::path(
    get,
    path = "/v1/admin/audit",
    params(
        ("actor" = Option<Uuid>, Query, description = "Only rows this user acted on"),
        ("action" = Option<String>, Query, description = "Only rows with this exact dotted action name"),
        ("target" = Option<String>, Query, description = "Only rows targeting this id (any target type)"),
        ("from" = Option<String>, Query, description = "Only rows at or after this instant (RFC 3339, inclusive)"),
        ("to" = Option<String>, Query, description = "Only rows before this instant (RFC 3339, exclusive)"),
        ("limit" = Option<u32>, Query, description = "Page size, clamped to 1..=100 (default 20)"),
        ("cursor" = Option<String>, Query, description = "Opaque keyset from the previous page's next_cursor")
    ),
    responses(
        (status = 200, description = "One page of audit rows, newest first", body = AuditPage),
        (status = 401, description = "Unauthenticated (no credential, bad or expired token)", body = Problem),
        (status = 403, description = "Forbidden (verified plain user, or an API key)", body = Problem),
        (status = 422, description = "Tampered cursor, malformed uuid filter, or malformed from/to", body = Problem)
    ),
    tag = "admin",
    security(("bearer_auth" = []))
)]
pub async fn admin_audit_handler(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    _admin: AdminReadContext,
    Query(params): Query<AuditQuery>,
) -> Response {
    let request_id = Some(id.0.as_str());
    match list_audit(
        &state,
        AuditFilters {
            actor_user_id: params.actor,
            action: params.action,
            target_id: params.target,
            from: params.from,
            to: params.to,
        },
        params.limit,
        params.cursor,
    )
    .await
    {
        Ok(page) => Json(page).into_response(),
        Err(AdminAuditError::InvalidCursor) => ProblemResponse(
            AppError::ValidationFailed.to_problem(Profile::Production, request_id, None),
        )
        .into_response(),
        Err(AdminAuditError::Db(db_err)) => {
            tracing::error!(error = %db_err, "admin audit: read failed");
            ProblemResponse(AppError::Internal.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
    }
}

/// Query params for `GET /v1/admin/abuse-reports`
/// (P1-090): all optional. `status` filters to one queue
/// state (`open` | `reviewing` | `resolved` — an unknown
/// value is 422, not an empty page); `user_id` filters to
/// reports naming that user (P1-098 — the attestation
/// review queue links here per user; an unknown user is an
/// empty page, not a 404; a malformed uuid is 422 via the
/// `Query` extractor); `limit` clamps to 1..=100 (default
/// 20); `cursor` is the opaque keyset from the previous
/// page.
#[derive(Debug, Deserialize)]
pub struct AbuseQueueQuery {
    pub status: Option<String>,
    pub user_id: Option<Uuid>,
    pub limit: Option<u32>,
    pub cursor: Option<String>,
}

/// GET /v1/admin/abuse-reports (P1-090): one page of the
/// abuse queue, newest first. Admin + support (the
/// read-only role) may call it; a verified plain user gets
/// 403 `PF-1002` from the extractor, no credential gets 401
/// `PF-1001`, an API key gets 403. Answers the api-spec
/// pagination shape (`{"items": [...], "next_cursor":
/// "..."}` — `next_cursor` omitted on the last page). Every
/// item carries the SLA pair: `sla_due_at` (`created_at` +
/// 72 h) and `sla_breached` (a resolved report: its
/// `resolved_at` is past the due time; an open or
/// reviewing one: the clock is past it). An unknown
/// `status` or a tampered cursor is 422 `PF-2002`. No audit
/// row (a read creates nothing).
#[utoipa::path(
    get,
    path = "/v1/admin/abuse-reports",
    params(
        ("status" = Option<String>, Query, description = "Only reports in this state: open | reviewing | resolved"),
        ("limit" = Option<u32>, Query, description = "Page size, clamped to 1..=100 (default 20)"),
        ("cursor" = Option<String>, Query, description = "Opaque keyset from the previous page's next_cursor")
    ),
    responses(
        (status = 200, description = "One page of abuse reports, newest first", body = AbuseQueuePage),
        (status = 401, description = "Unauthenticated (no credential, bad or expired token)", body = Problem),
        (status = 403, description = "Forbidden (verified plain user, or an API key)", body = Problem),
        (status = 422, description = "Unknown status filter or tampered cursor", body = Problem)
    ),
    tag = "admin",
    security(("bearer_auth" = []))
)]
pub async fn admin_abuse_queue_handler(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    _admin: AdminReadContext,
    Query(params): Query<AbuseQueueQuery>,
) -> Response {
    let request_id = Some(id.0.as_str());
    match list_abuse_queue(
        &state,
        params.status,
        params.user_id,
        params.limit,
        params.cursor,
    )
    .await
    {
        Ok(page) => Json(page).into_response(),
        Err(AbuseQueueError::InvalidStatus | AbuseQueueError::InvalidCursor) => ProblemResponse(
            AppError::ValidationFailed.to_problem(Profile::Production, request_id, None),
        )
        .into_response(),
        Err(AbuseQueueError::Internal) => {
            tracing::error!("admin abuse queue: list failed");
            ProblemResponse(AppError::Internal.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
        // The list takes no body and no target id — the
        // resolve-only variants are unreachable here.
        Err(
            AbuseQueueError::Validation(_)
            | AbuseQueueError::NotFound
            | AbuseQueueError::ActionNotApplicable,
        ) => ProblemResponse(AppError::Internal.to_problem(Profile::Production, request_id, None))
            .into_response(),
    }
}

/// POST /v1/admin/abuse-reports/{id}/resolve (P1-090):
/// act on one report. Admin only — support is read-only
/// and gets 403 `PF-1002` from the extractor, no credential
/// gets 401 `PF-1001`, an API key gets 403. The body is
/// `{"action": "start_review" | "reject_job" |
/// "suspend_user" | "dismiss", "reason": "...", "notes"?}`:
/// `start_review` picks the report up (open → reviewing),
/// the other three resolve it (`dismissed` |
/// `job_rejected` | `user_suspended`) with the resolution
/// stamps (`resolved_at` from the DB clock, `resolved_by`
/// the admin). `reject_job` sets the reported job's status
/// to `rejected`; `suspend_user` runs the full P1-008
/// sequence on the reported user. Every action is audited
/// in the same transaction as the mutation (the AC):
/// `admin.abuse_reports.resolved` for the three terminal
/// actions, `abuse.report_reviewing` for the pickup, plus
/// `user.suspended` for a suspension — so the handler
/// marks the response `AuditHandled` and the P1-046
/// middleware stands down. Idempotent: resolving an
/// already-resolved report (or picking up an
/// already-reviewing one) is a 200 with the report as it
/// stands — no write, no audit. An unknown report id is
/// 404 `PF-2010`; an action the report cannot take
/// (`reject_job` with no job, `suspend_user` with no user)
/// is 422 `PF-2002`; a bad reason/notes is 422 with the
/// field list (the `ValidatedJson` extractor).
#[utoipa::path(
    post,
    path = "/v1/admin/abuse-reports/{id}/resolve",
    request_body = ResolveAbuseReportRequest,
    responses(
        (status = 200, description = "Resolved (or already resolved — the report as it stands)", body = AbuseReportItem),
        (status = 401, description = "Unauthenticated (no credential, bad or expired token)", body = Problem),
        (status = 403, description = "Forbidden (verified non-admin, including support)", body = Problem),
        (status = 404, description = "No such report, or the named job/user no longer exists", body = Problem),
        (status = 422, description = "Bad reason/notes, an action that does not apply to this report, or an unknown action", body = Problem)
    ),
    tag = "admin",
    security(("bearer_auth" = []))
)]
pub async fn resolve_abuse_report_handler(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    admin: AdminContext,
    Path(report_id): Path<Uuid>,
    ValidatedJson(req): ValidatedJson<ResolveAbuseReportRequest>,
) -> Response {
    let request_id = Some(id.0.as_str());
    let Some(admin_user) = admin.auth.user else {
        // The extractor guarantees a user for a verified context.
        return ProblemResponse(AppError::Internal.to_problem(
            Profile::Production,
            request_id,
            None,
        ))
        .into_response();
    };
    match resolve_abuse_report(&state, &admin_user, report_id, &req).await {
        Ok(outcome) => {
            // The service wrote the audit row(s) in the same
            // transaction as the mutation (P1-090) — mark the
            // response so the P1-046 middleware stands down and
            // the row exists exactly once (also on the
            // idempotent no-op, which deliberately writes none).
            let mut response = Json(outcome.report).into_response();
            response.extensions_mut().insert(AuditHandled);
            response
        }
        Err(AbuseQueueError::Validation(errors)) => {
            ProblemResponse(errors.into_problem(Profile::Production, request_id)).into_response()
        }
        Err(AbuseQueueError::NotFound) => {
            ProblemResponse(AppError::NotFound.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
        Err(
            AbuseQueueError::InvalidStatus
            | AbuseQueueError::InvalidCursor
            | AbuseQueueError::ActionNotApplicable,
        ) => ProblemResponse(AppError::ValidationFailed.to_problem(
            Profile::Production,
            request_id,
            None,
        ))
        .into_response(),
        Err(AbuseQueueError::Internal) => {
            ProblemResponse(AppError::Internal.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
    }
}

/// Query params for `GET /v1/admin/attestations`
/// (P1-098): all optional. `flagged` filters to flagged
/// rows only (`true`), unflagged only (`false`), or all
/// rows (absent — any other value is 422, not an empty
/// page); `limit` clamps to 1..=100 (default 20); `cursor`
/// is the opaque keyset from the previous page.
#[derive(Debug, Deserialize)]
pub struct AttestationsQuery {
    pub flagged: Option<String>,
    pub limit: Option<u32>,
    pub cursor: Option<String>,
}

/// GET /v1/admin/attestations (P1-098): one page of the
/// attestation review queue, newest first. Admin + support
/// (the read-only role) may call it; a verified plain user
/// gets 403 `PF-1002` from the extractor, no credential
/// gets 401 `PF-1001`, an API key gets 403. Answers the
/// api-spec pagination shape (`{"items": [...],
/// "next_cursor": "..."}` — `next_cursor` omitted on the
/// last page). Every item carries the velocity flag:
/// `velocity_count` (the user's attestation count in the
/// 24 h window ending at that row's `created_at`),
/// `flagged` (the count exceeds 10), `flag_reasons`
/// (`["velocity"]` when flagged, `[]` otherwise), and
/// `abuse_queue_url` (the abuse queue for that user, a
/// relative reference). An unknown `flagged` value or a
/// tampered cursor is 422 `PF-2002`. No audit row (a read
/// creates nothing).
#[utoipa::path(
    get,
    path = "/v1/admin/attestations",
    params(
        ("flagged" = Option<String>, Query, description = "Only flagged rows (true), only unflagged rows (false), or all rows when absent"),
        ("limit" = Option<u32>, Query, description = "Page size, clamped to 1..=100 (default 20)"),
        ("cursor" = Option<String>, Query, description = "Opaque keyset from the previous page's next_cursor")
    ),
    responses(
        (status = 200, description = "One page of attestations, newest first", body = AttestationsPage),
        (status = 401, description = "Unauthenticated (no credential, bad or expired token)", body = Problem),
        (status = 403, description = "Forbidden (verified plain user, or an API key)", body = Problem),
        (status = 422, description = "Unknown flagged filter or tampered cursor", body = Problem)
    ),
    tag = "admin",
    security(("bearer_auth" = []))
)]
pub async fn admin_attestations_handler(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    _admin: AdminReadContext,
    Query(params): Query<AttestationsQuery>,
) -> Response {
    let request_id = Some(id.0.as_str());
    match list_attestations(&state, params.flagged, params.limit, params.cursor).await {
        Ok(page) => Json(page).into_response(),
        Err(AttestationReviewError::InvalidFlagged | AttestationReviewError::InvalidCursor) => {
            ProblemResponse(AppError::ValidationFailed.to_problem(
                Profile::Production,
                request_id,
                None,
            ))
            .into_response()
        }
        Err(AttestationReviewError::Internal) => {
            tracing::error!("attestation review: list failed");
            ProblemResponse(AppError::Internal.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
    }
}

/// The guarded 501 stubs (P1-043): the same 501 `PF-5004` as
/// the shared [`stub`](super::stub), but behind the matrix
/// guard — so a route whose handler has not landed yet still
/// enforces authorization at the boundary (no credential is
/// 401 `PF-1001`, a wrong role or a key is 403 `PF-1002`,
/// before the stub ever runs). The two wrappers differ only
/// in which guard type the call site reads as: write stubs
/// take `admin_stub` (`AdminContext`), read stubs take
/// `admin_read_stub` (`AdminReadContext`) — the matrix, not
/// the type, decides who gets in (P1-042), so a read stub
/// admits admin + support and a write stub admits admin only.
/// The handler ticket replaces the wrapper with its real
/// handler, keeping the same guard type. A stub writes no
/// audit row: it changes no state, and the audit trail
/// records actions, not attempts — denials are proven to
/// audit nothing by `apps/api/tests/admin_guard.rs`.
async fn admin_stub(Extension(id): Extension<RequestId>, _admin: AdminContext) -> ProblemResponse {
    let problem = AppError::NotImplemented.to_problem(Profile::Production, Some(&id.0), None);
    ProblemResponse(problem)
}

async fn admin_read_stub(
    Extension(id): Extension<RequestId>,
    _admin: AdminReadContext,
) -> ProblemResponse {
    let problem = AppError::NotImplemented.to_problem(Profile::Production, Some(&id.0), None);
    ProblemResponse(problem)
}

/// File an abuse report (P1-089, FR-C-06). Public: no
/// credential is checked at all — the reporter is anonymous
/// by design (the matrix row is `Ignored`: a key changes
/// nothing, the anonymous and keyed responses are identical).
/// Thin handler: the service owns the flow; this maps its
/// errors to the problem+json envelope. Returns 201 with the
/// ticket (`{id, status, created_at}`); a honeypot-filled
/// submission gets the same 201 shape with a random id, so a
/// bot cannot tell a dropped submission from a filed one.
/// A bad body is 422 `PF-2002` (the `ValidatedJson`
/// extractor); a named job id that does not exist is 404
/// `PF-2010`; a tripped per-IP bucket is 429 `PF-3007`
/// with `Retry-After` + the bucket's `X-RateLimit-*` trio.
#[utoipa::path(
    post,
    path = "/v1/abuse",
    request_body = AbuseIntakeRequest,
    responses(
        (status = 201, description = "Report filed; the ticket the reporter holds", body = AbuseReportResponse),
        (status = 404, description = "The named job id does not exist", body = Problem),
        (status = 422, description = "Validation failed (contact email shape, reason 1-500 chars)", body = Problem),
        (status = 429, description = "Too many abuse reports from this address", body = Problem)
    ),
    tag = "admin"
)]
pub async fn abuse_intake(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    headers: HeaderMap,
    MaybeConnectInfo(addr): MaybeConnectInfo,
    ValidatedJson(req): ValidatedJson<AbuseIntakeRequest>,
) -> Response {
    let request_id = Some(id.0.as_str());
    match file_abuse_report(&state, &req, &client_ip_from(&headers, addr)).await {
        Ok(AbuseIntakeOutcome::Filed(report)) => {
            (StatusCode::CREATED, Json(report)).into_response()
        }
        Ok(AbuseIntakeOutcome::HoneypotDropped) => {
            // Indistinguishable 201: a random id + now, the
            // same shape as a filed report. Nothing was
            // written — no row, no audit, no mail.
            let dropped = AbuseReportResponse {
                id: Uuid::now_v7(),
                status: "open".to_string(),
                created_at: state.clock.now(),
            };
            (StatusCode::CREATED, Json(dropped)).into_response()
        }
        Err(AbuseIntakeError::RateLimited(result, limit)) => {
            denied_response(result, limit, request_id)
        }
        Err(AbuseIntakeError::UnknownJob) => {
            ProblemResponse(AppError::NotFound.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
        Err(AbuseIntakeError::Internal) => {
            ProblemResponse(AppError::Internal.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
    }
}

/// The admin nest: dashboard, users (search / suspend / restore /
/// quota-grant / impersonate), jobs (browse / reprocess / cancel),
/// flags, plans, audit, abuse reports, attestations, system —
/// plus the public abuse intake at `/v1/abuse`.
///
/// The nest carries the P1-046 audit middleware
/// ([`audit_admin_writes`]): a successful admin write gets
/// exactly one audit row from the route metadata, unless the
/// handler marked its response [`AuditHandled`] (the live
/// writes do — their services write the row in the mutation's
/// transaction). Reads and non-2xx responses audit nothing.
/// The middleware needs the state ([`middleware::from_fn`]
/// cannot extract `State`, so it is
/// [`middleware::from_fn_with_state`] with a clone), which is
/// why this router takes it.
pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .nest(
            "/v1/admin",
            Router::new()
                .route("/metrics/dashboard", get(admin_read_stub))
                .route("/users", get(search_users_handler))
                .route("/users/{id}", get(admin_user_view_handler))
                .route("/users/{id}/suspend", post(suspend))
                .route("/users/{id}/restore", post(restore))
                .route("/users/{id}/warn", post(warn_user_handler))
                .route("/users/{id}/terminate", post(terminate_user_handler))
                .route("/users/{id}/quota-grant", post(quota_grant_handler))
                .route("/users/{id}/impersonate", post(impersonate))
                .route("/jobs", get(list_jobs_handler))
                .route("/jobs/{id}/reprocess", post(admin_stub))
                .route("/jobs/{id}/cancel", post(admin_stub))
                .route("/dlq", get(admin_read_stub))
                .route("/flags", get(admin_read_stub).put(admin_stub))
                .route("/plans", get(list_plans_handler).put(update_plans_handler))
                .route("/billing/catalog-sync", post(catalog_sync_handler))
                .route(
                    "/billing/subscriptions/{id}/cancel",
                    post(cancel_subscription),
                )
                .route(
                    "/billing/subscriptions/{id}/refund",
                    post(refund_tool_handler),
                )
                .route("/usage/{id}", get(admin_usage_handler))
                .route("/usage/{id}/export", get(admin_usage_export_handler))
                .route("/audit", get(admin_audit_handler))
                .route("/abuse-reports", get(admin_abuse_queue_handler))
                .route(
                    "/abuse-reports/{id}/resolve",
                    post(resolve_abuse_report_handler),
                )
                .route("/attestations", get(admin_attestations_handler))
                .route("/system", get(admin_read_stub))
                .layer(middleware::from_fn_with_state(state, audit_admin_writes)),
        )
        .nest("/v1/abuse", Router::new().route("/", post(abuse_intake)))
}
