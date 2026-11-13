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
    auth::AdminContext,
    problem::ProblemResponse,
    services::{
        admin::{restore_user, suspend_user, AdminError, SuspendRequest},
        register::UserResponse,
    },
    AppState,
};
use axum::{
    extract::{Extension, Path, State},
    response::{IntoResponse, Json, Response},
    routing::{get, post},
    Router,
};
use detag_core::error::{AppError, Problem, Profile};
use uuid::Uuid;

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
    request_body = SuspendRequest,
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
    ValidatedJson(req): ValidatedJson<SuspendRequest>,
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
        Err(AdminError::Internal) => {
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
        // Restore takes no body, so Validation is unreachable
        // here — it shares the enum with suspend.
        Err(AdminError::Validation(_) | AdminError::Internal) => {
            ProblemResponse(AppError::Internal.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
        Err(AdminError::NotFound) => {
            ProblemResponse(AppError::NotFound.to_problem(Profile::Production, request_id, None))
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
                .route("/users", get(stub))
                .route("/users/{id}/suspend", post(suspend))
                .route("/users/{id}/restore", post(restore))
                .route("/users/{id}/quota-grant", post(stub))
                .route("/users/{id}/impersonate", post(stub))
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
