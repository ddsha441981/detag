//! Session routes (P1-019): `GET /v1/me/sessions` +
//! `DELETE /v1/me/sessions/{id}`. Thin handlers: the service
//! owns the flow; these map its errors to the problem+json
//! envelope. Both take the Bearer `AuthContext` — the caller
//! is the user whose sessions they are. Handlers return
//! `Response` directly (not `Result<_, ProblemResponse>`):
//! `ProblemResponse` is 152 bytes, and clippy's
//! `result_large_err` (deny) forbids it as the `Err` of a
//! `Result` whose `Ok` is small — every real handler follows
//! this shape; see the adding-an-endpoint doc.

use super::extractors::RequestId;
use crate::{
    auth::AuthContext,
    problem::ProblemResponse,
    services::sessions::{
        list_sessions as list_sessions_service, revoke_session, SessionError, SessionItem,
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

/// List the user's live sessions (P1-019, FR-A-01): device
/// (parsed), created, last_seen, expiry — newest first.
/// Revoked and expired sessions are not listed (they are
/// history, in `audit_logs`); the user can only act on a
/// live one.
#[utoipa::path(
    get,
    path = "/v1/me/sessions",
    responses(
        (status = 200, description = "Live sessions, newest first", body = Vec<SessionItem>),
        (status = 401, description = "No credential, or an invalid/expired access token (PF-1001/1010/1011)", body = Problem)
    ),
    tag = "auth"
)]
pub async fn list_sessions(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    auth: AuthContext,
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
    match list_sessions_service(&state, user).await {
        Ok(items) => (StatusCode::OK, Json(items)).into_response(),
        // The only error the list can produce is Internal
        // (`NotFound` belongs to the revoke path — an empty
        // list is `[]`, not an error).
        Err(_) => {
            ProblemResponse(AppError::Internal.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
    }
}

/// Revoke one of the user's sessions (P1-019, FR-A-01).
/// Unknown id, or an id belonging to another user → 404
/// `PF-2010` (no enumeration). Already revoked → no-op 204
/// (the session is already dead — P1-008's convention).
#[utoipa::path(
    delete,
    path = "/v1/me/sessions/{id}",
    params(
        ("id" = Uuid, Path, description = "The session id from GET /v1/me/sessions")
    ),
    responses(
        (status = 204, description = "Session revoked (or already revoked)"),
        (status = 401, description = "No credential, or an invalid/expired access token (PF-1001/1010/1011)", body = Problem),
        (status = 404, description = "No session with that id for this user (PF-2010)", body = Problem)
    ),
    tag = "auth"
)]
pub async fn delete_session(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    auth: AuthContext,
    Path(session_id): Path<Uuid>,
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
    match revoke_session(&state, user, actor, session_id).await {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(SessionError::NotFound) => {
            ProblemResponse(AppError::NotFound.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
        Err(SessionError::Internal) => {
            ProblemResponse(AppError::Internal.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
    }
}
