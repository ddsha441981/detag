//! P0-101: the input validation layer, end to end.
//!
//! The validator crate's rules (email, filename sanitize,
//! public-only URL) reach the wire through `ValidatedJson`: a
//! request DTO that fails validation is rejected with a 422
//! problem+json body carrying `field_errors` — one entry per
//! failed field. The probe below is test-local (no production
//! route takes a JSON body yet — the stubs take only
//! `Extension<RequestId>`), layered with the real request-id +
//! error-mapper middleware in production order, exactly like the
//! 415 probe in `error_contract.rs`. Phase 1 handlers adopt
//! `ValidatedJson` when they land (see `04-ENGINEERING/
//! 04-adding-an-endpoint.md`).

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
    middleware,
    response::Response,
    routing::post,
    Router,
};
use detag_api::{
    error_mapper::error_mapper_middleware, problem::PROBLEM_CONTENT_TYPE,
    request_id::request_id_middleware, routes::extractors::ValidatedJson,
};
use detag_validator::{validate_email, validate_public_url, Validate, ValidationErrors};
use serde::Deserialize;
use tower::ServiceExt;

/// Fixed request id (a valid UUID, so the middleware echoes it).
const REQUEST_ID: &str = "123e4567-e89b-42d3-a456-426614174000";

/// Probe DTO: the three P0-101 rules on one body. `filename` is
/// sanitized (not merely validated) — the cleaned name is what a
/// real handler would store; here we only prove it passes.
#[derive(Debug, Deserialize)]
struct RegisterProbe {
    email: String,
    filename: String,
    webhook_url: Option<String>,
}

impl Validate for RegisterProbe {
    fn validate(&self) -> Result<(), ValidationErrors> {
        let mut errors = ValidationErrors::new();
        if let Err(e) = validate_email("email", &self.email) {
            errors.push(e);
        }
        if let Err(e) = detag_validator::sanitize_filename("filename", &self.filename) {
            errors.push(e);
        }
        if let Some(url) = &self.webhook_url {
            if let Err(e) = validate_public_url("webhook_url", url) {
                errors.push(e);
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

async fn probe(ValidatedJson(_body): ValidatedJson<RegisterProbe>) -> &'static str {
    "ok"
}

fn probe_router() -> Router {
    Router::new()
        .route("/probe", post(probe))
        .layer(middleware::from_fn(error_mapper_middleware))
        .layer(middleware::from_fn(request_id_middleware))
}

async fn post_probe(body: &str, content_type: &str) -> Response {
    probe_router()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/probe")
                .header("content-type", content_type)
                .header("x-request-id", REQUEST_ID)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .expect("probe request runs")
}

async fn body_json(res: Response) -> serde_json::Value {
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .expect("read body");
    serde_json::from_slice(&body).expect("body is JSON")
}

fn assert_problem_envelope(json: &serde_json::Value, status: u16) {
    assert_eq!(json["type"], "https://docs.pureframe.dev/errors/PF-2002");
    assert_eq!(json["title"], "validation_failed");
    assert_eq!(json["status"], status);
    assert_eq!(json["code"], "PF-2002");
    assert_eq!(json["detail"], "the request failed validation");
    assert_eq!(json["request_id"], REQUEST_ID);
}

#[tokio::test]
async fn valid_body_passes() {
    let res = post_probe(
        r#"{"email":"alice@example.com","filename":"photo.png","webhook_url":"https://example.com/hook"}"#,
        "application/json",
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn sanitized_filename_passes() {
    // Traversal is cleaned, not rejected — the handler stores the
    // sanitized name. (The probe only proves acceptance; the
    // sanitize unit tests pin the cleaned value.)
    let res = post_probe(
        r#"{"email":"alice@example.com","filename":"../photo.png"}"#,
        "application/json",
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn bad_email_is_422_with_field_error() {
    let res = post_probe(
        r#"{"email":"not-an-email","filename":"photo.png"}"#,
        "application/json",
    )
    .await;
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        res.headers().get("content-type").unwrap().to_str().unwrap(),
        PROBLEM_CONTENT_TYPE
    );
    let json = body_json(res).await;
    assert_problem_envelope(&json, 422);
    let errors = json["field_errors"]
        .as_array()
        .expect("field_errors is a list");
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0]["field"], "email");
    assert_eq!(errors[0]["code"], "invalid_email");
    assert_eq!(errors[0]["message"], "must be a valid email address");
}

#[tokio::test]
async fn private_webhook_url_is_422() {
    let res = post_probe(
        r#"{"email":"alice@example.com","filename":"photo.png","webhook_url":"http://169.254.169.254/latest/"}"#,
        "application/json",
    )
    .await;
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let json = body_json(res).await;
    assert_problem_envelope(&json, 422);
    let errors = json["field_errors"]
        .as_array()
        .expect("field_errors is a list");
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0]["field"], "webhook_url");
    assert_eq!(errors[0]["code"], "private_url");
}

#[tokio::test]
async fn all_bad_fields_listed_in_one_body() {
    let res = post_probe(
        r#"{"email":"bad","filename":"..","webhook_url":"http://localhost/hook"}"#,
        "application/json",
    )
    .await;
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let json = body_json(res).await;
    assert_problem_envelope(&json, 422);
    let errors = json["field_errors"]
        .as_array()
        .expect("field_errors is a list");
    assert_eq!(errors.len(), 3, "one entry per failed field: {errors:?}");
    assert_eq!(errors[0]["field"], "email");
    assert_eq!(errors[1]["field"], "filename");
    assert_eq!(errors[2]["field"], "webhook_url");
}

#[tokio::test]
async fn malformed_json_is_422_without_field_errors() {
    // Nothing parsed, so there are no fields to report — the plain
    // ValidationFailed body, same as the error mapper produces.
    let res = post_probe(r#"{"email": "alice@example.com""#, "application/json").await;
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let json = body_json(res).await;
    assert_problem_envelope(&json, 422);
    assert!(
        json.get("field_errors").is_none(),
        "malformed body has no field_errors: {json}"
    );
}
