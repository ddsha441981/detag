//! Error mapper (P0-065): framework errors become problem+json.
//!
//! Handlers return `AppError`, which the stub and (from Phase 1)
//! the handlers render through [`ProblemResponse`] — those bodies
//! already are problem+json and pass through untouched. But axum
//! itself generates responses the handlers never see: 404 (no
//! route matched), 413 (the body-limit layer rejected the body),
//! 415 (a `Json` extractor rejected a non-JSON content type),
//! and 400/422 (an extractor rejected the request: bad JSON, bad
//! query). Left alone those are empty or plain-text bodies with
//! no `PF-` code — a client cannot tell them apart, and the
//! plain-text variants can echo user input. The mapper rewrites
//! exactly those five statuses into the problem envelope:
//!
//! | status | `AppError` | code |
//! | ------ | ---------- | ---- |
//! | 404 | `NotFound` | PF-2010 |
//! | 400, 422 | `ValidationFailed` | PF-2002 |
//! | 413 | `PayloadTooLarge` | PF-2003 |
//! | 415 | `UnsupportedMediaType` | PF-2001 |
//!
//! Deliberately NOT mapped: 405 (no taxonomy variant; axum's
//! `Allow` header is worth keeping verbatim) and 5xx (a handler
//! 500 is already problem+json; a framework 500 without a body
//! is a bug, and fabricating a PF code for it would hide it).
//! The original body is discarded, never forwarded — the
//! framework's plain-text rejections can contain the offending
//! input, and an error body must never be a PII leak.
//!
//! Position matters (see the layer table in `lib.rs`): the mapper
//! is the INNERMOST middleware, inside the body-limit layer, so
//! it sees the 413 the limit generates; CORS, secure headers,
//! and request-id sit outside it, so the rewritten response
//! still gets `Access-Control-Allow-Origin`, the secure-header
//! table, and `X-Request-Id` on its way out.

use crate::problem::{ProblemResponse, PROBLEM_CONTENT_TYPE};
use crate::routes::extractors::RequestId;
use axum::{
    body::Body,
    http::{header, Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use detag_core::error::{AppError, Profile};

/// Axum middleware: rewrite framework 400/404/413/415/422 responses
/// into the problem+json envelope (see the module docs).
pub async fn error_mapper_middleware(req: Request<Body>, next: Next) -> Response {
    let request_id = req.extensions().get::<RequestId>().map(|id| id.0.clone());
    let res = next.run(req).await;
    if !needs_mapping(&res) {
        return res;
    }
    let err = match res.status() {
        StatusCode::NOT_FOUND => AppError::NotFound,
        StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY => AppError::ValidationFailed,
        StatusCode::PAYLOAD_TOO_LARGE => AppError::PayloadTooLarge,
        StatusCode::UNSUPPORTED_MEDIA_TYPE => AppError::UnsupportedMediaType,
        _ => unreachable!("needs_mapping only admits 400/404/413/415/422"),
    };
    let problem = err.to_problem(Profile::Production, request_id.as_deref(), None);
    let (mut parts, _body) = res.into_parts();
    // The old body is dropped (see the module docs: it can echo
    // user input). Content-Length is stale for the new body and
    // Content-Type is replaced by the problem responder.
    parts.headers.remove(header::CONTENT_LENGTH);
    parts.headers.remove(header::CONTENT_TYPE);
    (parts, ProblemResponse(problem)).into_response()
}

/// True when the response is a framework error that must become a
/// problem body: status 400/404/413/415/422 and not already
/// problem+json.
fn needs_mapping(res: &Response) -> bool {
    matches!(
        res.status(),
        StatusCode::BAD_REQUEST
            | StatusCode::NOT_FOUND
            | StatusCode::PAYLOAD_TOO_LARGE
            | StatusCode::UNSUPPORTED_MEDIA_TYPE
            | StatusCode::UNPROCESSABLE_ENTITY
    ) && !is_problem(res)
}

/// True when the response already carries a problem body.
fn is_problem(res: &Response) -> bool {
    res.headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.starts_with(PROBLEM_CONTENT_TYPE))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::header::HeaderValue;

    fn response_with(status: StatusCode, content_type: Option<&str>) -> Response {
        let mut res = Response::new(Body::empty());
        *res.status_mut() = status;
        if let Some(ct) = content_type {
            res.headers_mut()
                .insert(header::CONTENT_TYPE, HeaderValue::from_str(ct).unwrap());
        }
        res
    }

    #[test]
    fn mapping_table() {
        // Framework plain-text errors map; problem bodies pass.
        assert!(needs_mapping(&response_with(StatusCode::NOT_FOUND, None)));
        assert!(needs_mapping(&response_with(
            StatusCode::NOT_FOUND,
            Some("text/plain; charset=utf-8")
        )));
        assert!(needs_mapping(&response_with(
            StatusCode::PAYLOAD_TOO_LARGE,
            None
        )));
        assert!(needs_mapping(&response_with(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Some("text/plain")
        )));
        assert!(!needs_mapping(&response_with(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Some("application/problem+json")
        )));
        assert!(needs_mapping(&response_with(
            StatusCode::BAD_REQUEST,
            Some("text/plain")
        )));
        assert!(needs_mapping(&response_with(
            StatusCode::UNPROCESSABLE_ENTITY,
            None
        )));
        assert!(!needs_mapping(&response_with(
            StatusCode::NOT_FOUND,
            Some("application/problem+json")
        )));
        assert!(!needs_mapping(&response_with(
            StatusCode::NOT_FOUND,
            Some("application/problem+json; charset=utf-8")
        )));
        // 405 and 500 are deliberately untouched.
        assert!(!needs_mapping(&response_with(
            StatusCode::METHOD_NOT_ALLOWED,
            None
        )));
        assert!(!needs_mapping(&response_with(
            StatusCode::INTERNAL_SERVER_ERROR,
            None
        )));
        // Successes pass through.
        assert!(!needs_mapping(&response_with(StatusCode::OK, None)));
    }
}
