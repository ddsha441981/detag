//! Auth routes (P0-064): `/v1/auth/*`, `/v1/me/*`, `/v1/api-keys/*`.
//!
//! API keys live here (they are credentials; the spec's six
//! modules leave them no other home). Handlers are the shared
//! 501 stub until their Phase 1 tickets land — except
//! `/v1/auth/register`, which is real since P1-001, and
//! `/v1/auth/verify-email` + `/v1/auth/resend-verification`,
//! real since P1-002.

use super::extractors::RequestId;
use super::stub;
use crate::{
    problem::ProblemResponse,
    services::{
        register::{register_user, RegisterError, RegisterRequest, UserResponse},
        verification::{
            resend_verification as resend_verification_service,
            verify_email as verify_email_service, ResendVerificationRequest, VerifyEmailRequest,
            VerifyError,
        },
    },
    AppState,
};
use axum::{
    extract::{Extension, State},
    http::StatusCode,
    response::{IntoResponse, Json, Response},
    routing::{delete, get, post},
    Router,
};
use detag_core::error::{AppError, Problem, Profile};

use crate::routes::extractors::ValidatedJson;

/// Register a user (P1-001, FR-A-01). Thin handler: the
/// service owns the flow; this maps its errors to the
/// problem+json envelope. Returns `Response` directly (not
/// `Result<_, ProblemResponse>`): `ProblemResponse` is 152
/// bytes, and clippy's `result_large_err` (deny) forbids it as
/// the `Err` of a `Result` whose `Ok` is small — and axum has
/// no `IntoResponse` for `Box<ProblemResponse>` either. Every
/// real handler follows this shape; see the adding-an-endpoint
/// doc.
#[utoipa::path(
    post,
    path = "/v1/auth/register",
    request_body = RegisterRequest,
    responses(
        (status = 201, description = "User created; verification email queued", body = UserResponse),
        (status = 409, description = "Email already registered (generic body — no enumeration detail)", body = Problem),
        (status = 422, description = "Validation failed (email shape, password length, ToS version, disposable domain)", body = Problem)
    ),
    tag = "auth"
)]
pub async fn register(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    ValidatedJson(req): ValidatedJson<RegisterRequest>,
) -> Response {
    let request_id = Some(id.0.as_str());
    match register_user(&state.pool, &state.settings, &req).await {
        Ok(user) => (StatusCode::CREATED, Json(UserResponse::from(user))).into_response(),
        Err(RegisterError::Duplicate) => {
            ProblemResponse(AppError::Conflict.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
        Err(RegisterError::Invalid(errors)) => {
            ProblemResponse(errors.into_problem(Profile::Production, request_id)).into_response()
        }
        Err(RegisterError::Internal) => {
            ProblemResponse(AppError::Internal.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
    }
}

/// Verify an email (P1-002, FR-A-01): consume the single-use
/// token from the verification link, set `email_verified_at`.
/// Unknown/used token → 401 PF-1010; expired token → 401
/// PF-1011. Success is 204 (no body — nothing to return).
#[utoipa::path(
    post,
    path = "/v1/auth/verify-email",
    request_body = VerifyEmailRequest,
    responses(
        (status = 204, description = "Email verified"),
        (status = 401, description = "Invalid or expired token", body = Problem),
        (status = 422, description = "Validation failed (token required)", body = Problem)
    ),
    tag = "auth"
)]
pub async fn verify_email(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    ValidatedJson(req): ValidatedJson<VerifyEmailRequest>,
) -> Response {
    let request_id = Some(id.0.as_str());
    match verify_email_service(&state.pool, &req.token).await {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(VerifyError::InvalidToken) => ProblemResponse(AppError::InvalidToken.to_problem(
            Profile::Production,
            request_id,
            None,
        ))
        .into_response(),
        Err(VerifyError::ExpiredToken) => ProblemResponse(AppError::ExpiredToken.to_problem(
            Profile::Production,
            request_id,
            None,
        ))
        .into_response(),
        Err(VerifyError::Internal) => {
            ProblemResponse(AppError::Internal.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
    }
}

/// Re-send a verification mail (P1-002, FR-A-01). This route
/// is NOT in the original api-spec — the spec says "resend
/// rate-limited (3/hour)" but names no route. Added as
/// `POST /v1/auth/resend-verification`, 204-always: unknown
/// email, already-verified user, and over-limit resends all
/// answer 204 with no mail (no enumeration, mirroring
/// forgot-password). Only a 500 varies the response.
#[utoipa::path(
    post,
    path = "/v1/auth/resend-verification",
    request_body = ResendVerificationRequest,
    responses(
        (status = 204, description = "If the email is registered and unverified (and under the hourly limit), a fresh verification mail was queued"),
        (status = 422, description = "Validation failed (email shape)", body = Problem)
    ),
    tag = "auth"
)]
pub async fn resend_verification(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    ValidatedJson(req): ValidatedJson<ResendVerificationRequest>,
) -> Response {
    let request_id = Some(id.0.as_str());
    // Only VerifyError::Internal is reachable (the service
    // never returns Invalid/ExpiredToken); any error is a 500.
    if let Err(err) = resend_verification_service(&state.pool, &req.normalized_email()).await {
        tracing::error!(error = ?err, "resend-verification failed");
        return ProblemResponse(AppError::Internal.to_problem(
            Profile::Production,
            request_id,
            None,
        ))
        .into_response();
    }
    StatusCode::NO_CONTENT.into_response()
}

/// The auth nests: registration/login/session management under
/// `/v1/auth`, the profile + sessions under `/v1/me`, API keys
/// under `/v1/api-keys`.
pub fn router() -> Router<AppState> {
    Router::new()
        .nest(
            "/v1/auth",
            Router::new()
                .route("/register", post(register))
                .route("/verify-email", post(verify_email))
                .route("/resend-verification", post(resend_verification))
                .route("/login", post(stub))
                .route("/refresh", post(stub))
                .route("/logout", post(stub))
                .route("/forgot-password", post(stub))
                .route("/reset-password", post(stub)),
        )
        .nest(
            "/v1/me",
            Router::new()
                .route("/", get(stub).patch(stub).delete(stub))
                .route("/sessions", get(stub))
                .route("/sessions/{id}", delete(stub))
                .route("/export", post(stub)),
        )
        .nest(
            "/v1/api-keys",
            Router::new()
                .route("/", get(stub).post(stub))
                .route("/{id}", delete(stub)),
        )
}
