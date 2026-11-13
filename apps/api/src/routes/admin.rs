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
//! and `/restore`, real since P1-008 (in this file).
//!
//! Both live handlers take [`AdminContext`]: the extractor
//! verifies the Bearer <redacted> and requires the role to be admin
//! before the handler body runs, so a non-admin (or an
//! unauthenticated caller) never reaches the service — 401
//! `PF-1001` without a credential, 403 `PF-1002` for a
//! verified non-admin. The handlers are thin: the admin
//! service owns the flow; this maps its errors to the
//! problem+json envelope. They return `Response` directly
//! (not `Result<_, ProblemResponse>` — the P1-001
//! `result_large_err` shape; see the adding-an-endpoint doc).

use super::{
    extractors::{RequestId, ValidatedJson},
    stub,
};
use crate::{
    auth::{AdminContext, AdminReadContext},
    problem::ProblemResponse,
    services::{
        admin::{
            impersonate_user, restore_user, search_users, suspend_user, AdminError,
            ImpersonateResponse, ReasonRequest, UserSearchPage,
        },
        register::UserResponse,
    },
    AppState,
};
use axum::{
    extract::{Extension, Path, Query, State},
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
        Ok(outcome) => Json(UserResponse::from(outcome.user)).into_response(),
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
        Ok(outcome) => Json(UserResponse::from(outcome.user)).into_response(),
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
        Ok(response) => Json(response).into_response(),
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

/// The admin nest: dashboard, users (search / suspend / restore /
/// quota-grant / impersonate), jobs (browse / reprocess / cancel),
/// flags, plans, audit, abuse reports, attestations, system —
/// plus the public abuse intake at `/v1/abuse`.
pub fn router() -> Router<AppState> {
    Router::new()
        .nest(
            "/v1/admin",
            Router::new()
                .route("/metrics/dashboard", get(stub))
                .route("/users", get(search_users_handler))
                .route("/users/{id}/suspend", post(suspend))
                .route("/users/{id}/restore", post(restore))
                .route("/users/{id}/quota-grant", post(stub))
                .route("/users/{id}/impersonate", post(impersonate))
                .route("/jobs", get(stub))
                .route("/jobs/{id}/reprocess", post(stub))
                .route("/jobs/{id}/cancel", post(stub))
                .route("/dlq", get(stub))
                .route("/flags", get(stub).put(stub))
                .route("/plans", get(stub).put(stub))
                .route("/audit", get(stub))
                .route("/abuse-reports", get(stub))
                .route("/abuse-reports/{id}/resolve", post(stub))
                .route("/attestations", get(stub))
                .route("/system", get(stub)),
        )
        .nest("/v1/abuse", Router::new().route("/", post(stub)))
}
