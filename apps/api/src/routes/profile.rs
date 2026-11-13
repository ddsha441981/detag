//! Profile routes (P1-005): GET /v1/me + PATCH /v1/me.
//!
//! Both take [`AuthContext`]: the extractor verifies the Bearer
//! JWT and rejects unverified callers with 401 before the handler
//! body runs, so `auth.user` is always `Some` here (a missing
//! user is a 500 — it means the extractor's invariant broke).
//! The handlers are thin: the profile service owns the flow; this
//! maps its errors to the problem+json envelope. They return
//! `Response` directly (not `Result<_, ProblemResponse>` — the
//! P1-001 `result_large_err` shape; see the adding-an-endpoint
//! doc).

use super::extractors::{RequestId, ValidatedJson};
use crate::{
    auth::AuthContext,
    problem::ProblemResponse,
    services::profile::{
        get_profile, patch_profile, PatchProfileRequest, ProfileError, ProfileResponse,
    },
    AppState,
};
use axum::{
    extract::{Extension, State},
    response::{IntoResponse, Json, Response},
};
use detag_core::error::{AppError, Problem, Profile};

/// GET /v1/me (P1-005): the profile + the current plan + today's
/// quota snapshot. The snapshot is the entitlements service's
/// reading (plan entitlements vs today's usage row) — the same
/// two sources the quota middleware (P1-051) reads, so the
/// snapshot and the middleware agree by construction.
#[utoipa::path(
    get,
    path = "/v1/me",
    responses(
        (status = 200, description = "Profile with plan and quota snapshot", body = ProfileResponse),
        (status = 401, description = "Unauthenticated (no credential, bad or expired token)", body = Problem)
    ),
    tag = "auth",
    security(("bearer_auth" = []))
)]
pub async fn get_me(State(state): State<AppState>, auth: AuthContext) -> Response {
    let Some(user) = auth.user else {
        // The extractor guarantees a user for a verified context.
        return ProblemResponse(AppError::Internal.to_problem(Profile::Production, None, None))
            .into_response();
    };
    match get_profile(&state, &user).await {
        Ok(profile) => Json(profile).into_response(),
        Err(ProfileError::Internal) => {
            ProblemResponse(AppError::Internal.to_problem(Profile::Production, None, None))
                .into_response()
        }
        // GET cannot fail validation or hit a duplicate.
        Err(ProfileError::Validation(_) | ProfileError::Duplicate) => {
            ProblemResponse(AppError::Internal.to_problem(Profile::Production, None, None))
                .into_response()
        }
    }
}

/// PATCH /v1/me (P1-005): set the display name and/or record an
/// email-change intent. An email value stores `pending_email`
/// (validated + disposable-checked + duplicate-checked) — P1-006
/// verifies and swaps it. `email: null` clears a pending intent;
/// `display_name: null` clears the name; no fields is a no-op 200.
#[utoipa::path(
    patch,
    path = "/v1/me",
    request_body = PatchProfileRequest,
    responses(
        (status = 200, description = "Updated profile", body = ProfileResponse),
        (status = 401, description = "Unauthenticated (no credential, bad or expired token)", body = Problem),
        (status = 409, description = "Email already registered", body = Problem),
        (status = 422, description = "Validation failed (display name shape, email shape, disposable domain, email unchanged)", body = Problem)
    ),
    tag = "auth",
    security(("bearer_auth" = []))
)]
pub async fn patch_me(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    auth: AuthContext,
    ValidatedJson(req): ValidatedJson<PatchProfileRequest>,
) -> Response {
    let request_id = Some(id.0.as_str());
    let Some(user) = auth.user else {
        // The extractor guarantees a user for a verified context.
        return ProblemResponse(AppError::Internal.to_problem(
            Profile::Production,
            request_id,
            None,
        ))
        .into_response();
    };
    match patch_profile(&state, &user, &req).await {
        Ok(profile) => Json(profile).into_response(),
        Err(ProfileError::Duplicate) => {
            ProblemResponse(AppError::Conflict.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
        Err(ProfileError::Validation(errors)) => {
            ProblemResponse(errors.into_problem(Profile::Production, request_id)).into_response()
        }
        Err(ProfileError::Internal) => {
            ProblemResponse(AppError::Internal.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
    }
}
