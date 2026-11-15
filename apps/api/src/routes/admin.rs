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
//! /v1/admin/users/{id}/impersonate`, real since P1-010, and
//! `GET /v1/admin/jobs`, real since P1-038 (in this file).
//! Every route — live or stub — sits behind the matrix guard
//! (P1-043): the stubs take `admin_stub` / `admin_read_stub`
//! below, so an unauthenticated or wrong-role caller is
//! rejected before the 501, exactly like on the live routes.
//! The public abuse intake keeps the plain `stub` — it is not
//! an admin route.
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

use super::{
    extractors::{RequestId, ValidatedJson},
    stub,
};
use crate::{
    audit_middleware::{audit_admin_writes, AuditHandled},
    auth::{AdminContext, AdminReadContext},
    problem::ProblemResponse,
    services::{
        admin::{
            impersonate_user, list_jobs, restore_user, search_users, suspend_user, AdminError,
            AdminJobsPage, ImpersonateResponse, ReasonRequest, UserSearchPage,
        },
        register::UserResponse,
    },
    AppState,
};
use axum::{
    extract::{Extension, Path, Query, State},
    middleware,
    response::{IntoResponse, Json, Response},
    routing::{get, post},
    Router,
};
use detag_core::error::{AppError, Problem, Profile};
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

/// The admin nest: dashboard, users (search / suspend / restore /
/// quota-grant / impersonate), jobs (browse / reprocess / cancel),
/// flags, plans, audit, abuse reports, attestations, system —
/// plus the public abuse intake at `/v1/abuse`.
///
/// The nest carries the P1-046 audit middleware
/// ([`audit_admin_writes`]): a successful admin write gets
/// exactly one audit row from the route metadata, unless the
/// handler marked its response [`AuditHandled`] (the three live
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
                .route("/users/{id}/suspend", post(suspend))
                .route("/users/{id}/restore", post(restore))
                .route("/users/{id}/quota-grant", post(admin_stub))
                .route("/users/{id}/impersonate", post(impersonate))
                .route("/jobs", get(list_jobs_handler))
                .route("/jobs/{id}/reprocess", post(admin_stub))
                .route("/jobs/{id}/cancel", post(admin_stub))
                .route("/dlq", get(admin_read_stub))
                .route("/flags", get(admin_read_stub).put(admin_stub))
                .route("/plans", get(admin_read_stub).put(admin_stub))
                .route("/audit", get(admin_read_stub))
                .route("/abuse-reports", get(admin_read_stub))
                .route("/abuse-reports/{id}/resolve", post(admin_stub))
                .route("/attestations", get(admin_read_stub))
                .route("/system", get(admin_read_stub))
                .layer(middleware::from_fn_with_state(state, audit_admin_writes)),
        )
        .nest("/v1/abuse", Router::new().route("/", post(stub)))
}
