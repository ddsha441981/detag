//! Job routes (P0-064): `/v1/jobs/*`.
//!
//! All handlers are the authenticated 501 stub
//! (`stub_authed`, P1-034) until their Phase 2/3 tickets
//! land: a caller without a credential is 401 `PF-1001`, an
//! API key without the route's scope is 403 `PF-1002`, and a
//! verified caller reaches the stub's 501. The attestation
//! route (second explicit consent step for gated provenance
//! operations) lives here because it is job-scoped.
//!
//! P1-051: `POST /v1/jobs` is the exception — the quota
//! pre-check runs before the stub. A request that names an
//! `upload_id` is gated on the plan's daily images /
//! video-seconds and the concurrency slot (429 `PF-3001` /
//! `PF-3002` / `PF-3008`, problem+json with the upgrade
//! hint, `X-Quota-*` headers on the denial); a request
//! inside quota reaches the stub's 501 with the same
//! headers. A non-empty body that is not valid JSON is 422
//! `PF-2002` (the same problem the `Json` extractor used
//! to produce).
//!
//! P1-052: every job-creating response carries the
//! `X-Quota-*` headers — a request with no body, or no
//! `upload_id`, reaches the stub WITH the caller's headers
//! (both probe shapes: an empty body with no content-type
//! AND an empty body with `application/json` — the matrix
//! probe sends the latter), and `POST /v1/jobs/{id}/retry`
//! carries them too (the caller's snapshot; the handler
//! reads no job row, so an unknown id still 501s — the
//! 404 ownership proof lands with the real retry handler
//! in Phase 2, ASSUMPTION-018). The numbers come from the
//! same durable snapshot as the gate and as `/v1/me`, so
//! all three agree by construction (the UI contract is
//! `docs/06-DELIVERY/08-quota-ui-contract.md`).
//!
//! The body is read as raw bytes on purpose: axum's
//! `Option<Json<T>>` treats an empty body as "no body"
//! only when there is no JSON content-type — an empty body
//! WITH `application/json` is a `Json` rejection (422),
//! which broke the api_key_matrix probe (the verify for
//! P1-051 went red on exactly that shape). Reading bytes
//! makes the two "no body" shapes identical, and the
//! manual parse keeps the 422 shape for genuinely malformed
//! bodies.

use super::stub_authed;
use crate::auth::AuthContext;
use crate::problem::ProblemResponse;
use crate::services::entitlements::QuotaSnapshot;
use crate::services::quota::{
    self, QuotaError, QuotaOutcome, HEADER_IMAGES_REMAINING, HEADER_VIDEO_SECONDS_REMAINING,
};
use crate::AppState;
use axum::{
    body::Bytes,
    extract::{Extension, State},
    http::HeaderValue,
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use detag_core::error::{AppError, Profile};
use serde::Deserialize;
use uuid::Uuid;

use super::extractors::RequestId;

/// The job-create request body (P1-051): only `upload_id`
/// is read — the pre-check needs the upload to know the
/// kind and the duration. Every other spec field (mode,
/// hints, `format_policy`, `strip_metadata`, `provenance`,
/// `webhook_url`) is accepted and ignored until Phase 2
/// parses the real create body; unknown fields are ignored
/// too, so a Phase 2-shaped body already passes the gate.
#[derive(Debug, Deserialize)]
pub struct JobCreateRequest {
    #[serde(default)]
    pub upload_id: Option<Uuid>,
}

/// Stamp the quota headers from a snapshot onto a response:
/// today's images / video-seconds remaining (floored at 0
/// by the snapshot — never negative, even overshot).
fn with_quota_headers(mut res: Response, snapshot: &QuotaSnapshot) -> Response {
    res.headers_mut().insert(
        HEADER_IMAGES_REMAINING,
        HeaderValue::from(snapshot.daily_images.remaining as u64),
    );
    res.headers_mut().insert(
        HEADER_VIDEO_SECONDS_REMAINING,
        HeaderValue::from(snapshot.daily_video_seconds.remaining as u64),
    );
    res
}

/// The 501 stub as a response (the shared shape, rendered
/// here so the pre-check can stamp headers on it).
fn stub_response(request_id: Option<&str>) -> Response {
    ProblemResponse(AppError::NotImplemented.to_problem(Profile::Production, request_id, None))
        .into_response()
}

/// The stub's 501 with the caller's quota headers (P1-052):
/// every job-creating response carries the remaining quota,
/// gated or not. A snapshot failure is 500, like the gate's
/// own failure arm — the headers are data, not decoration.
async fn stub_with_headers(state: &AppState, user_id: Uuid, request_id: Option<&str>) -> Response {
    match quota::snapshot_for_user(state, user_id).await {
        Ok(snapshot) => with_quota_headers(stub_response(request_id), &snapshot),
        Err(err) => {
            tracing::error!(error = ?err, "quota snapshot failed");
            ProblemResponse(AppError::Internal.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
    }
}

/// `POST /v1/jobs` (P1-051): the quota pre-check, then the
/// stub. Auth runs first (the extractor: no credential is
/// 401, a key without `jobs:write` is 403 — the stub's
/// guarantees are unchanged); then, when the body names an
/// upload, the pre-check decides: a denial is the 429
/// problem+json with the `X-Quota-*` headers, an allow is
/// the stub's 501 with the same headers. No body (empty, with
/// or without a JSON content-type — see the module docs for
/// why the bytes are read raw), or no `upload_id`, reaches
/// the stub WITH the caller's quota headers (P1-052 — every
/// create response carries them; there is no job to check,
/// but the caller still sees what is left). A non-empty body
/// that does not parse as JSON is 422 `PF-2002`, the same
/// problem the `Json` extractor produced.
async fn create_job(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    auth: AuthContext,
    body: Bytes,
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
    if body.is_empty() || body.iter().all(|b| b.is_ascii_whitespace()) {
        return stub_with_headers(&state, user.id, request_id).await;
    }
    let req: JobCreateRequest = match serde_json::from_slice(&body) {
        Ok(req) => req,
        Err(_) => {
            return ProblemResponse(AppError::ValidationFailed.to_problem(
                Profile::Production,
                request_id,
                None,
            ))
            .into_response();
        }
    };
    let Some(upload_id) = req.upload_id else {
        return stub_with_headers(&state, user.id, request_id).await;
    };
    match quota::precheck_for_create(&state, user.id, upload_id).await {
        Ok(QuotaOutcome::Allowed(decision)) => {
            with_quota_headers(stub_response(request_id), &decision.snapshot)
        }
        Ok(QuotaOutcome::Denied(err, snapshot)) => {
            let problem = err.to_problem(Profile::Production, request_id, None);
            let res = ProblemResponse(problem).into_response();
            with_quota_headers(res, &snapshot)
        }
        Err(QuotaError::UploadNotFound) => {
            ProblemResponse(AppError::NotFound.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
        Err(err) => {
            tracing::error!(error = ?err, "quota pre-check failed");
            ProblemResponse(AppError::Internal.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
    }
}

/// `POST /v1/jobs/{id}/retry` (P1-052): the stub, with the
/// caller's quota headers. A retry creates a new run of the
/// job, so it counts against quota (api-spec) — the caller
/// sees what is left. The handler reads NO job row: the
/// 404 ownership proof (P1-045) lands with the real retry
/// handler in Phase 2, when the handler first reads the job
/// (ASSUMPTION-018). Auth runs first (the extractor: no
/// credential is 401, a key without `jobs:write` is 403).
async fn retry_job(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    auth: AuthContext,
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
    stub_with_headers(&state, user.id, request_id).await
}

/// The jobs nest: create, list, get, cancel, retry, SSE events,
/// result, report, attestation.
pub fn router() -> Router<AppState> {
    Router::new().nest(
        "/v1/jobs",
        Router::new()
            .route("/", post(create_job).get(stub_authed))
            .route("/{id}", get(stub_authed))
            .route("/{id}/cancel", post(stub_authed))
            .route("/{id}/retry", post(retry_job))
            .route("/{id}/events", get(stub_authed))
            .route("/{id}/result", get(stub_authed))
            .route("/{id}/report", get(stub_authed))
            .route("/{id}/attestation", post(stub_authed)),
    )
}
