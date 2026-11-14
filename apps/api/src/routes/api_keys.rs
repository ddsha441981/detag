//! API key routes (P1-033): `POST /v1/api-keys`. Thin handler:
//! the service owns the flow; this maps its errors to the
//! problem+json envelope. Takes the Bearer `AuthContext` —
//! the caller is the user the key belongs to. The full secret
//! is returned in the creation response only; there is no
//! endpoint that returns it again.
//!
//! Handlers return `Response` directly (not
//! `Result<_, ProblemResponse>`): `ProblemResponse` is 152
//! bytes, and clippy's `result_large_err` (deny) forbids it
//! as the `Err` of a `Result` whose `Ok` is small — every real
//! handler follows this shape; see the adding-an-endpoint doc.

use super::extractors::{RequestId, ValidatedJson};
use crate::{
    auth::AuthContext,
    problem::ProblemResponse,
    services::api_keys::{
        create_api_key as create_api_key_service, rotate_api_key as rotate_api_key_service,
        ApiKeyError, CreateApiKeyRequest, CreateApiKeyResponse, RotateApiKeyError,
    },
    AppState,
};
use axum::{
    extract::{Extension, Path, State},
    http::StatusCode,
    response::{IntoResponse, Json, Response},
};
use detag_core::error::{AppError, Problem, Profile};
use uuid::Uuid;

/// Mint an API key (P1-033, FR-A-06): `pf_live_` + 24 random
/// bytes, sha256 stored, full secret shown once in this
/// response. Max 10 live keys per user — the 11th is 409
/// `PF-2011` (Conflict: the request is valid, the user's
/// state refuses it).
#[utoipa::path(
    post,
    path = "/v1/api-keys",
    request_body = CreateApiKeyRequest,
    responses(
        (status = 201, description = "Key created; the full secret is shown here once and never again", body = CreateApiKeyResponse),
        (status = 401, description = "No credential, or an invalid/expired access token (PF-1001/1010/1011)", body = Problem),
        (status = 409, description = "User already has 10 live keys (PF-2011)", body = Problem),
        (status = 422, description = "Invalid name or scopes (PF-2002)", body = Problem)
    ),
    tag = "auth"
)]
pub async fn create_api_key(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    auth: AuthContext,
    ValidatedJson(req): ValidatedJson<CreateApiKeyRequest>,
) -> Response {
    let request_id = Some(id.0.as_str());
    let Some(ref user) = auth.user else {
        return ProblemResponse(AppError::Unauthenticated.to_problem(
            Profile::Production,
            request_id,
            None,
        ))
        .into_response();
    };
    let actor = auth.audit_actor_id(user);
    match create_api_key_service(&state, user, actor, &req).await {
        Ok(created) => (StatusCode::CREATED, Json(created)).into_response(),
        Err(ApiKeyError::Validation(errors)) => {
            ProblemResponse(errors.into_problem(Profile::Production, request_id)).into_response()
        }
        Err(ApiKeyError::CapReached) => {
            ProblemResponse(AppError::Conflict.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
        Err(ApiKeyError::Internal) => {
            ProblemResponse(AppError::Internal.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
    }
}

/// Rotate an API key (P1-035, FR-A-06): `POST
/// /v1/api-keys/{id}/rotate` mints a replacement secret
/// (same name + scopes) and keeps the old secret valid for
/// the overlap window (`PF_API_KEY_ROTATION_OVERLAP_SECS`,
/// default 24 h). The response carries the NEW secret once —
/// the same shape as creation; the old secret is never
/// shown again. A key cannot rotate (itself or any other):
/// the route is not in the extractor's scope table, so a
/// key caller is 403 `PF-1002` before this handler runs.
/// Unknown id, another user's id, or a revoked key are all
/// 404 `PF-2010` (no enumeration).
#[utoipa::path(
    post,
    path = "/v1/api-keys/{id}/rotate",
    responses(
        (status = 201, description = "Key rotated; the new secret is shown here once and never again", body = CreateApiKeyResponse),
        (status = 401, description = "No credential, or an invalid/expired access token (PF-1001/1010/1011)", body = Problem),
        (status = 403, description = "An API key cannot rotate (PF-1002)", body = Problem),
        (status = 404, description = "Unknown id, another user's key, or a revoked key (PF-2010)", body = Problem)
    ),
    tag = "auth"
)]
pub async fn rotate_api_key(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    auth: AuthContext,
    Path(key_id): Path<Uuid>,
) -> Response {
    let request_id = Some(id.0.as_str());
    let Some(ref user) = auth.user else {
        return ProblemResponse(AppError::Unauthenticated.to_problem(
            Profile::Production,
            request_id,
            None,
        ))
        .into_response();
    };
    let actor = auth.audit_actor_id(user);
    match rotate_api_key_service(&state, user, actor, key_id).await {
        Ok(rotated) => (StatusCode::CREATED, Json(rotated)).into_response(),
        Err(RotateApiKeyError::NotFound) => {
            ProblemResponse(AppError::NotFound.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
        Err(RotateApiKeyError::Internal) => {
            ProblemResponse(AppError::Internal.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
    }
}
