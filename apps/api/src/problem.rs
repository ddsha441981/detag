//! The problem+json responder (P0-065).
//!
//! The error taxonomy lives in `detag-core` (`AppError::to_problem`
//! builds the RFC 7807 body), but core has no axum dependency, so
//! the wire shape lives here: [`ProblemResponse`] renders a
//! [`Problem`](detag_core::error::Problem) with the real
//! `application/problem+json` content type. Before this ticket the
//! stub rendered the same body through axum `Json`, which sets
//! `application/json` — the body was right, the media type was
//! not. Every error body the api emits (stubs, handlers, the
//! error mapper) goes through this type, so clients can rely on
//! the content type, not just the shape.

use axum::{
    body::Body,
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use detag_core::error::Problem;

/// The media type of every error body the api emits.
pub const PROBLEM_CONTENT_TYPE: &str = "application/problem+json";

/// A [`Problem`] rendered as an HTTP response: status from the
/// problem, body the problem JSON, content type problem+json.
pub struct ProblemResponse(pub Problem);

impl IntoResponse for ProblemResponse {
    fn into_response(self) -> Response {
        let status =
            StatusCode::from_u16(self.0.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let body = serde_json::to_vec(&self.0).expect("Problem is always serializable");
        let mut res = Response::new(Body::from(body));
        *res.status_mut() = status;
        res.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static(PROBLEM_CONTENT_TYPE),
        );
        res
    }
}
