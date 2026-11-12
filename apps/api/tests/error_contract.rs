//! P0-073: the error contract, locked as golden tests.
//!
//! Every error body the api emits is RFC 7807 problem+json with
//! exactly the fields `{type, title, status, code, detail,
//! request_id}` — nothing more, nothing less — the `PF-` code in
//! its documented range, and the request id echoed. This file
//! asserts that shape for 401, 415, 429 and 500 (plus a 404
//! control) with hand-typed expected bodies: the literals below
//! are the independent oracle (api spec + error taxonomy), so a
//! drift in any `code()` / `status()` / `title()` /
//! `default_detail()` arm fails on the exact row.
//!
//! Two legs:
//! - Live, through the real router: 415 (a `Json` extractor
//!   rejects a non-JSON content type; the error mapper rewrites
//!   it to PF-2001) and 404 (no route matched; PF-2010). The 415
//!   probe is a test-local route layered with the real
//!   `request_id` + `error_mapper` middleware in production
//!   order — no production route takes a JSON body yet (the
//!   stubs take only `Extension<RequestId>`), so the full app
//!   router cannot produce a framework 415 today.
//! - Responder-level, through [`ProblemResponse`] (the exact type
//!   every handler renders through): 401, 429 and 500 have no
//!   live path in Phase 0 — the auth extractor never rejects
//!   (P0-070), no rate-limit layer is wired yet, and no handler
//!   returns 500 — so their shapes are locked here, and the
//!   Phase 1 endpoint tests must add the live legs when those
//!   paths land (register/login for 401, the rate limiter for
//!   429, a failing handler for 500).

mod common;

use axum::{
    body::Body,
    extract::Json,
    http::{Method, Request, StatusCode},
    middleware,
    response::{IntoResponse, Response},
    routing::post,
    Router,
};
use common::spawn_app;
use detag_api::{
    error_mapper::error_mapper_middleware,
    problem::{ProblemResponse, PROBLEM_CONTENT_TYPE},
    request_id::request_id_middleware,
};
use detag_core::error::{AppError, Profile};
use tower::ServiceExt;

/// Fixed request id for the golden bodies (a valid UUID, so the
/// request-id middleware echoes it instead of generating one).
const REQUEST_ID: &str = "123e4567-e89b-42d3-a456-426614174000";

/// The `PF-` range the code for this status must fall in
/// (error taxonomy: PF-1xxx auth, PF-2xxx validation/media,
/// PF-3xxx quota/rate-limit, PF-5xxx system).
fn code_range(status: u16) -> std::ops::Range<u32> {
    match status {
        401 => 1000..2000,
        415 => 2000..3000,
        429 => 3000..4000,
        500 => 5000..6000,
        404 => 2000..3000,
        _ => unreachable!("golden test only covers 401/404/415/429/500"),
    }
}

/// Assert the whole error contract for one response: the status,
/// the problem+json content type, the exact golden body, and the
/// code range for the status.
async fn assert_golden(res: Response, status: u16, code: &str, title: &str, detail: &str) {
    assert_eq!(
        res.status(),
        StatusCode::from_u16(status).expect("test status is valid"),
        "{code}: HTTP status must be {status}"
    );
    let content_type = res
        .headers()
        .get("content-type")
        .unwrap_or_else(|| panic!("{code}: content-type present"))
        .to_str()
        .expect("content-type is ascii")
        .to_string();
    assert_eq!(
        content_type, PROBLEM_CONTENT_TYPE,
        "{code}: content type must be problem+json"
    );
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .expect("read body");
    let json: serde_json::Value = serde_json::from_slice(&body)
        .unwrap_or_else(|err| panic!("{code}: body must be JSON: {err}"));
    let expected = serde_json::json!({
        "type": format!("https://docs.pureframe.dev/errors/{code}"),
        "title": title,
        "status": status,
        "code": code,
        "detail": detail,
        "request_id": REQUEST_ID,
    });
    assert_eq!(json, expected, "{code}: golden body mismatch");
    let n: u32 = code[3..].parse().expect("numeric code");
    assert!(
        code_range(status).contains(&n),
        "{code}: code must be in the range for status {status}"
    );
}

/// Render one `AppError` exactly the way a handler does, and lock
/// its golden body. `code`/`title`/`detail` are typed by hand
/// from the taxonomy — the oracle, not the code under test.
async fn assert_responder_golden(
    err: AppError,
    status: u16,
    code: &str,
    title: &str,
    detail: &str,
) {
    let problem = err.to_problem(Profile::Production, Some(REQUEST_ID), None);
    assert_eq!(problem.code, code, "taxonomy drift for {code}");
    assert_eq!(problem.status, status, "taxonomy drift for {code}");
    assert_eq!(problem.title, title, "taxonomy drift for {code}");
    assert_eq!(problem.detail, detail, "taxonomy drift for {code}");
    let res = ProblemResponse(problem).into_response();
    assert_golden(res, status, code, title, detail).await;
}

#[tokio::test]
async fn golden_401_variants() {
    assert_responder_golden(
        AppError::Unauthenticated,
        401,
        "PF-1001",
        "unauthenticated",
        "authentication is required for this resource",
    )
    .await;
    assert_responder_golden(
        AppError::InvalidToken,
        401,
        "PF-1010",
        "invalid_token",
        "the access token is invalid",
    )
    .await;
    assert_responder_golden(
        AppError::ExpiredToken,
        401,
        "PF-1011",
        "expired_token",
        "the access token has expired",
    )
    .await;
}

#[tokio::test]
async fn golden_415_live_through_mapper() {
    // A Json-extracting probe behind the real middleware, in
    // production order (request-id outside the mapper).
    async fn json_probe(_: Json<serde_json::Value>) -> &'static str {
        "ok"
    }
    let router = Router::new()
        .route("/probe", post(json_probe))
        .layer(middleware::from_fn(error_mapper_middleware))
        .layer(middleware::from_fn(request_id_middleware));
    let res = router
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/probe")
                .header("content-type", "text/plain")
                .header("x-request-id", REQUEST_ID)
                .body(Body::from("not json"))
                .unwrap(),
        )
        .await
        .expect("probe request runs");
    assert_golden(
        res,
        415,
        "PF-2001",
        "unsupported_media_type",
        "this media type is not accepted",
    )
    .await;
}

#[tokio::test]
async fn golden_429_variants() {
    assert_responder_golden(
        AppError::RateLimited,
        429,
        "PF-3007",
        "rate_limited",
        "too many requests; retry after the reset window",
    )
    .await;
    assert_responder_golden(
        AppError::QuotaImagesExceeded,
        429,
        "PF-3001",
        "quota_images_exceeded",
        "image quota exceeded; upgrade your plan for more",
    )
    .await;
}

#[tokio::test]
async fn golden_500() {
    assert_responder_golden(
        AppError::Internal,
        500,
        "PF-5000",
        "internal_error",
        "an internal error occurred",
    )
    .await;
}

#[tokio::test]
async fn golden_404_live_control() {
    let app = spawn_app().await;
    let res = app
        .request(
            Request::builder()
                .method(Method::GET)
                .uri("/nope")
                .header("x-request-id", REQUEST_ID)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    // The harness response is a TestResponse, not an axum Response;
    // assert the same contract field by field.
    assert_eq!(res.status, StatusCode::NOT_FOUND, "PF-2010: status");
    assert_eq!(
        res.headers
            .get("content-type")
            .expect("content-type present")
            .to_str()
            .expect("content-type is ascii"),
        PROBLEM_CONTENT_TYPE,
        "PF-2010: content type"
    );
    let json = res.json();
    assert_eq!(
        json,
        serde_json::json!({
            "type": "https://docs.pureframe.dev/errors/PF-2010",
            "title": "not_found",
            "status": 404,
            "code": "PF-2010",
            "detail": "the requested resource was not found",
            "request_id": REQUEST_ID,
        }),
        "PF-2010: golden body mismatch"
    );
    assert_eq!(
        res.headers
            .get("x-request-id")
            .expect("x-request-id present")
            .to_str()
            .expect("request id is ascii"),
        REQUEST_ID
    );
}
