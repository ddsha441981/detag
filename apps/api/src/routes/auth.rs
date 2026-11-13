//! Auth routes (P0-064): `/v1/auth/*`, `/v1/me/*`, `/v1/api-keys/*`.
//!
//! API keys live here (they are credentials; the spec's six
//! modules leave them no other home). Handlers are the shared
//! 501 stub until their Phase 1 tickets land — except
//! `/v1/auth/register`, which is real since P1-001,
//! `/v1/auth/verify-email` + `/v1/auth/resend-verification`,
//! real since P1-002, `/v1/auth/login`, real since P1-003,
//! `POST /v1/auth/confirm-email-change`, real since P1-006,
//! `POST /v1/auth/refresh`, real since P1-014,
//! `POST /v1/auth/logout` + `/v1/auth/logout-all`, real since
//! P1-017, `GET /v1/me/sessions` +
//! `DELETE /v1/me/sessions/{id}`, real since P1-019 — in
//! `routes/sessions.rs`, the handlers live there, the nest
//! lives here — and `GET`/`PATCH`/`DELETE /v1/me`, real since
//! P1-005 (GET + PATCH) and P1-007 (DELETE) — in
//! `routes/profile.rs`, the handlers live there, the nest
//! lives here.

use super::extractors::RequestId;
use super::stub;
use crate::{
    auth::{parse_session_cookie, Credential},
    problem::ProblemResponse,
    rate_limit::{client_ip_from, denied_response},
    services::{
        login::{
            login_user, DeviceInfo, LoginError, LoginRequest, LoginResponse, REFRESH_COOKIE_NAME,
        },
        logout::{logout_all as logout_all_service, logout_current, LogoutError},
        refresh::{refresh_session, RefreshError},
        register::{register_user, RegisterError, RegisterRequest, UserResponse},
        verification::{
            confirm_email_change as confirm_email_change_service,
            resend_verification as resend_verification_service,
            verify_email as verify_email_service, ConfirmEmailChangeRequest, ConfirmError,
            ResendVerificationRequest, VerifyEmailRequest, VerifyError,
        },
    },
    AppState,
};
use axum::{
    extract::{Extension, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Json, Response},
    routing::{delete, get, post},
    Router,
};
use detag_core::error::{AppError, Problem, Profile};
use detag_core::jwt::REFRESH_TOKEN_TTL_SECS;

use crate::routes::extractors::{MaybeConnectInfo, ValidatedJson};

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

/// Confirm an email change (P1-006, FR-A-01): consume the
/// `email_change` token from the confirmation link mailed to
/// the NEW address, swap the email to the pending address
/// (clearing the intent, verifying the new address). Unknown /
/// used / wrong-purpose token → 401 PF-1010; expired token →
/// 401 PF-1011; an address taken by another account while it
/// was pending → 409 PF-2011. Success is 204 (no body —
/// the client re-reads the profile with GET /v1/me).
#[utoipa::path(
    post,
    path = "/v1/auth/confirm-email-change",
    request_body = ConfirmEmailChangeRequest,
    responses(
        (status = 204, description = "Email swapped to the pending address; new address verified"),
        (status = 401, description = "Invalid or expired token", body = Problem),
        (status = 409, description = "Pending address taken by another account meanwhile", body = Problem),
        (status = 422, description = "Validation failed (token required)", body = Problem)
    ),
    tag = "auth"
)]
pub async fn confirm_email_change(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    ValidatedJson(req): ValidatedJson<ConfirmEmailChangeRequest>,
) -> Response {
    let request_id = Some(id.0.as_str());
    match confirm_email_change_service(&state.pool, &req.token).await {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(ConfirmError::InvalidToken) => ProblemResponse(AppError::InvalidToken.to_problem(
            Profile::Production,
            request_id,
            None,
        ))
        .into_response(),
        Err(ConfirmError::ExpiredToken) => ProblemResponse(AppError::ExpiredToken.to_problem(
            Profile::Production,
            request_id,
            None,
        ))
        .into_response(),
        Err(ConfirmError::Duplicate) => {
            ProblemResponse(AppError::Conflict.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
        Err(ConfirmError::Internal) => {
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

/// Log a user in (P1-003, FR-A-01). Thin handler: the service
/// owns the flow; this maps its errors to the problem+json
/// envelope and sets the refresh cookie. Returns `Response`
/// directly (the P1-001 handler shape — clippy
/// `result_large_err` forbids `Result<_, ProblemResponse>`).
///
/// The device info comes from the request itself: the
/// `User-Agent` header and the socket address
/// (`MaybeConnectInfo` — the never-rejecting extractor; oneshot
/// tests have no socket, so the IP is `"unknown"` there). No
/// XFF: the device record is the socket IP, not a spoofable
/// header.
#[utoipa::path(
    post,
    path = "/v1/auth/login",
    request_body = LoginRequest,
    responses(
        (status = 200, description = "Logged in; access token in the body, refresh token in the httpOnly cookie", body = LoginResponse),
        (status = 401, description = "Invalid email or password (generic — no enumeration)", body = Problem),
        (status = 422, description = "Validation failed (email shape, password required)", body = Problem),
        (status = 429, description = "Too many login attempts for this account", body = Problem)
    ),
    tag = "auth"
)]
pub async fn login(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    headers: HeaderMap,
    MaybeConnectInfo(addr): MaybeConnectInfo,
    ValidatedJson(req): ValidatedJson<LoginRequest>,
) -> Response {
    let request_id = Some(id.0.as_str());
    let device = DeviceInfo {
        user_agent: headers
            .get(header::USER_AGENT)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string),
        ip: addr
            .map(|socket| socket.ip().to_string())
            .unwrap_or_else(|| "unknown".to_string()),
    };
    match login_user(&state, &req, &device, &client_ip_from(&headers, addr)).await {
        Ok(outcome) => {
            let cookie = format!(
                "{REFRESH_COOKIE_NAME}={}; HttpOnly; Secure; SameSite=Lax; Path=/v1/auth; Max-Age={REFRESH_TOKEN_TTL_SECS}",
                outcome.refresh_token
            );
            let mut res = (StatusCode::OK, Json(outcome.response)).into_response();
            if let Ok(value) = HeaderValue::from_str(&cookie) {
                res.headers_mut().insert(header::SET_COOKIE, value);
            }
            res
        }
        Err(LoginError::Invalid) => ProblemResponse(AppError::Unauthenticated.to_problem(
            Profile::Production,
            request_id,
            None,
        ))
        .into_response(),
        // 403 PF-1003 (P1-008): the account is suspended. Only
        // a correct password reaches here — a wrong password is
        // the generic 401 above, so probing learns nothing.
        Err(LoginError::Suspended) => ProblemResponse(AppError::AccountSuspended.to_problem(
            Profile::Production,
            request_id,
            None,
        ))
        .into_response(),
        Err(LoginError::Locked { retry_after_secs }) => {
            // 423 PF-1008 (P1-004). Same problem+json envelope
            // as every error, plus Retry-After — the client
            // knows exactly how long to wait before retrying.
            let mut res = ProblemResponse(AppError::AccountLocked.to_problem(
                Profile::Production,
                request_id,
                None,
            ))
            .into_response();
            res.headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from(retry_after_secs));
            res
        }
        Err(LoginError::RateLimited(result)) => {
            denied_response(result, state.rate_limit.login_account_burst(), request_id)
        }
        Err(LoginError::Internal) => {
            ProblemResponse(AppError::Internal.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
    }
}

/// Refresh the access token (P1-014, FR-A-01). Thin handler:
/// the service owns the flow; this maps its errors to the
/// problem+json envelope and sets the new refresh cookie.
/// The refresh token comes from the `detag_refresh` cookie
/// only — there is no body (a client that lost the cookie
/// logs in again). The device info is built the same way as
/// login (the `User-Agent` header + the socket address;
/// oneshot tests have no socket, so the IP is `"unknown"`
/// there). An impersonation token in the cookie is not a
/// refresh token — there is no session row behind it, so the
/// hash lookup fails with the same generic 401 (P1-010's
/// promise: P1-014's refresh rejects it).
#[utoipa::path(
    post,
    path = "/v1/auth/refresh",
    responses(
        (status = 200, description = "Rotated; new access token in the body, new refresh token in the httpOnly cookie", body = LoginResponse),
        (status = 401, description = "No cookie, or unknown / revoked / expired refresh token (PF-1001, generic); a rotated token reused is PF-1007 (whole family revoked)", body = Problem)
    ),
    tag = "auth"
)]
pub async fn refresh(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    headers: HeaderMap,
    MaybeConnectInfo(addr): MaybeConnectInfo,
) -> Response {
    let request_id = Some(id.0.as_str());
    let token = match parse_session_cookie(&headers) {
        Some(Credential::Session(token)) => token,
        // No cookie (or a cookie that is not a session
        // cookie): no credential — the generic 401.
        _ => {
            return ProblemResponse(AppError::Unauthenticated.to_problem(
                Profile::Production,
                request_id,
                None,
            ))
            .into_response();
        }
    };
    let device = DeviceInfo {
        user_agent: headers
            .get(header::USER_AGENT)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string),
        ip: addr
            .map(|socket| socket.ip().to_string())
            .unwrap_or_else(|| "unknown".to_string()),
    };
    match refresh_session(&state, &token, &device).await {
        Ok(outcome) => {
            let cookie = format!(
                "{REFRESH_COOKIE_NAME}={}; HttpOnly; Secure; SameSite=Lax; Path=/v1/auth; Max-Age={REFRESH_TOKEN_TTL_SECS}",
                outcome.refresh_token
            );
            let mut res = (StatusCode::OK, Json(outcome.response)).into_response();
            if let Ok(value) = HeaderValue::from_str(&cookie) {
                res.headers_mut().insert(header::SET_COOKIE, value);
            }
            res
        }
        // 401 PF-1007 (P1-014): a rotated token presented
        // again — the family is revoked, the audit + security
        // email are queued. The code is the signal: PF-1001
        // means "dead token", PF-1007 means "stolen token".
        Err(RefreshError::Reuse) => ProblemResponse(AppError::RefreshReuse.to_problem(
            Profile::Production,
            request_id,
            None,
        ))
        .into_response(),
        Err(RefreshError::Invalid) => ProblemResponse(AppError::Unauthenticated.to_problem(
            Profile::Production,
            request_id,
            None,
        ))
        .into_response(),
        Err(RefreshError::Internal) => {
            ProblemResponse(AppError::Internal.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
    }
}

/// The value that clears the refresh cookie (P1-017): the
/// same name, path, and flags as the login/refresh cookie,
/// with an empty value and `Max-Age=0` — the browser drops
/// it on receipt. Both logout handlers set it on success
/// (the client must not keep presenting a dead token).
fn clear_refresh_cookie() -> HeaderValue {
    HeaderValue::from_static(
        "detag_refresh=; HttpOnly; Secure; SameSite=Lax; Path=/v1/auth; Max-Age=0",
    )
}

/// Log out of the presented session (P1-017, FR-A-01). Thin
/// handler: the service owns the flow; this maps its errors
/// to the problem+json envelope and clears the refresh
/// cookie. The session is the one behind the `detag_refresh`
/// cookie — there is no body, and a Bearer token alone
/// cannot log one session out (the access token carries no
/// session identity; its `jti` is per-issuance, not linked
/// to a session row). No cookie, or an unknown hash → the
/// generic 401 `PF-1001` (the lookup must not say whether
/// the session existed). Logging out twice is a no-op 204
/// (the session is already dead — P1-008's convention).
#[utoipa::path(
    post,
    path = "/v1/auth/logout",
    responses(
        (status = 204, description = "Presented session revoked; the refresh cookie is cleared"),
        (status = 401, description = "No cookie, or unknown refresh token (PF-1001, generic)", body = Problem)
    ),
    tag = "auth"
)]
pub async fn logout(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    let request_id = Some(id.0.as_str());
    let token = match parse_session_cookie(&headers) {
        Some(Credential::Session(token)) => token,
        // No cookie (or a cookie that is not a session
        // cookie): no credential — the generic 401.
        _ => {
            return ProblemResponse(AppError::Unauthenticated.to_problem(
                Profile::Production,
                request_id,
                None,
            ))
            .into_response();
        }
    };
    match logout_current(&state, &token).await {
        Ok(_) => {
            let mut res = StatusCode::NO_CONTENT.into_response();
            res.headers_mut()
                .insert(header::SET_COOKIE, clear_refresh_cookie());
            res
        }
        Err(LogoutError::Invalid) => ProblemResponse(AppError::Unauthenticated.to_problem(
            Profile::Production,
            request_id,
            None,
        ))
        .into_response(),
        Err(LogoutError::Internal) => {
            ProblemResponse(AppError::Internal.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
    }
}

/// Log out of every session (P1-017, FR-A-01). Thin handler:
/// the service owns the flow; this maps its errors to the
/// problem+json envelope and clears the refresh cookie. The
/// caller is the Bearer `AuthContext` — the access token
/// identifies the USER (not the session), so the only safe
/// scope is every session. A cookie-only caller gets 401
/// `PF-1001` (the extractor rejects a bare cookie — P1-016):
/// the web client sends both (Bearer in memory + the cookie
/// the browser attaches), an API client sends the Bearer,
/// and a caller with only the cookie uses `POST
/// /v1/auth/logout` for the single session.
#[utoipa::path(
    post,
    path = "/v1/auth/logout-all",
    responses(
        (status = 204, description = "Every session revoked; the refresh cookie is cleared"),
        (status = 401, description = "No credential, or an invalid/expired access token (PF-1001/1010/1011)", body = Problem)
    ),
    tag = "auth"
)]
pub async fn logout_all(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    auth: crate::auth::AuthContext,
) -> Response {
    let request_id = Some(id.0.as_str());
    // A verified context always carries its user (P1-005) —
    // the `let Some(ref user)` keeps `auth` usable for the
    // audit actor (P1-011's E0382 lesson).
    let Some(ref user) = auth.user else {
        return ProblemResponse(AppError::Unauthenticated.to_problem(
            Profile::Production,
            request_id,
            None,
        ))
        .into_response();
    };
    let actor = auth.audit_actor_id(user);
    match logout_all_service(&state, user, actor).await {
        Ok(_) => {
            let mut res = StatusCode::NO_CONTENT.into_response();
            res.headers_mut()
                .insert(header::SET_COOKIE, clear_refresh_cookie());
            res
        }
        Err(LogoutError::Invalid) => ProblemResponse(AppError::Unauthenticated.to_problem(
            Profile::Production,
            request_id,
            None,
        ))
        .into_response(),
        Err(LogoutError::Internal) => {
            ProblemResponse(AppError::Internal.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
    }
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
                .route("/confirm-email-change", post(confirm_email_change))
                .route("/login", post(login))
                .route("/refresh", post(refresh))
                .route("/logout", post(logout))
                .route("/logout-all", post(logout_all))
                .route("/forgot-password", post(stub))
                .route("/reset-password", post(stub)),
        )
        .nest(
            "/v1/me",
            Router::new()
                .route(
                    "/",
                    get(super::profile::get_me)
                        .patch(super::profile::patch_me)
                        .delete(super::profile::delete_me),
                )
                .route("/sessions", get(super::sessions::list_sessions))
                .route("/sessions/{id}", delete(super::sessions::delete_session))
                .route("/export", post(stub)),
        )
        .nest(
            "/v1/api-keys",
            Router::new()
                .route("/", get(stub).post(stub))
                .route("/{id}", delete(stub)),
        )
}
