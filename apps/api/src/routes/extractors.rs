//! The shared extractor set (P0-064).
//!
//! Every route module takes its handler inputs from here —
//! never a module-local extractor type. The set today:
//!
//! - [`State`]`<`[`AppState`]`>` — the process state (pool,
//!   redis, storage, settings, clock). Every handler that
//!   touches a dependency takes it.
//! - `Extension<`[`RequestId`]`>` — the request id the
//!   request-id middleware assigned (and inserted into the
//!   request extensions). Handlers that render an error body
//!   take it so the body carries the same id as the response
//!   header.
//! - [`Path`] / [`Query`] — axum's own path-param and
//!   query-string extractors, re-exported so modules import
//!   extractors from one place.
//! - [`AuthContext`] — who the caller is (P0-070): which
//!   credential was presented (Bearer JWT, API key, or web
//!   session cookie) and the caller's [`Role`]. Phase 0 parses
//!   but does NOT verify — `verified` is always `false` and the
//!   role always [`Role::Anonymous`]; Phase 1 adds verification
//!   and the 401 rejections. See `crate::auth` for the full
//!   Phase 0 contract before using it.
//! - [`ValidatedJson`] — a JSON body that is also validated
//!   (P0-101): parses the body like axum's `Json`, then runs the
//!   DTO's [`Validate`] impl. Both failure modes render a 422
//!   problem+json body directly: a failed validation carries
//!   `field_errors`; a malformed body carries none (nothing
//!   parsed, so there are no fields to report) — the same body
//!   the error mapper would have produced for the framework
//!   rejection, so the mapper passes it through untouched.
//!
//! Do not invent a per-module auth extractor: a handler that
//! needs the caller takes [`AuthContext`] from this module.

pub use crate::auth::{AuthContext, Credential, Role};
pub use crate::request_id::RequestId;
pub use crate::AppState;
pub use axum::extract::{Path, Query, State};

use crate::problem::ProblemResponse;
use axum::extract::{FromRequest, Request};
use detag_core::error::Profile;
use detag_validator::Validate;
use serde::de::DeserializeOwned;

/// A JSON request body plus validation (P0-101).
///
/// Handlers take `ValidatedJson<T>` instead of `Json<T>` when the
/// DTO implements [`Validate`]. The rejection is a [`ProblemResponse`]:
/// a malformed body becomes a plain 422 `ValidationFailed` problem
/// (no field errors), a failed validation becomes the 422 problem
/// with `field_errors`.
pub struct ValidatedJson<T>(pub T);

impl<T, S> FromRequest<S> for ValidatedJson<T>
where
    T: DeserializeOwned + Validate,
    S: Send + Sync,
{
    type Rejection = ProblemResponse;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let request_id = req.extensions().get::<RequestId>().map(|id| id.0.clone());
        let failed = |request_id: &Option<String>| {
            ProblemResponse(detag_core::error::AppError::ValidationFailed.to_problem(
                Profile::Production,
                request_id.as_deref(),
                None,
            ))
        };
        // Parse first: a malformed body is a plain validation
        // failure (no field errors — nothing parsed). Reuse axum's
        // Json extractor so the parse behaviour stays identical to
        // every other route.
        let axum::extract::Json(value) = axum::extract::Json::<T>::from_request(req, state)
            .await
            .map_err(|_| failed(&request_id))?;
        match value.validate() {
            Ok(()) => Ok(Self(value)),
            Err(errors) => Err(ProblemResponse(
                errors.into_problem(Profile::Production, request_id.as_deref()),
            )),
        }
    }
}
