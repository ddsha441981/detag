//! P0-065: the middleware order test.
//!
//! The chain (outer → inner) is Extension<Metrics> → request-id
//! → CORS → secure headers → http-metrics → error mapper → routes
//! (body limit per-module, uploads exempt). The order is asserted
//! here through what each layer leaves on the response — not
//! through the code that builds the chain, which would be
//! circular. Each assertion pins one adjacency:
//!
//! - preflight carries `X-Request-Id` but NO secure headers:
//!   request-id is outside CORS (it ran), secure headers are
//!   inside CORS (the preflight short-circuited before them).
//! - a real request carries ACAO + the secure-header table +
//!   `X-Request-Id`: all three wrap the routes.
//! - a 404 and a 413 both come back as problem+json AND still
//!   carry ACAO + secure headers + `X-Request-Id`: the error
//!   mapper is inside CORS / secure headers / request-id, and
//!   inside the body limit (it sees the 413 the limit makes).
//! - a 2 MiB POST to `/v1/uploads` is NOT 413: the uploads nest
//!   is exempt from the limit.

use axum::{
    body::Body,
    http::{header, Method, Request, StatusCode},
};
use detag_api::{body_limit::MAX_JSON_BODY_BYTES, secure_headers::SECURE_HEADERS};
use detag_api::{build_router, AppState};
use detag_core::config::Settings;
use detag_core::metrics::Metrics;
use tower::ServiceExt;

/// The console origin used by the tests (never a real origin).
const ORIGIN: &str = "https://console.example.test";
/// A wrong origin: must never get an ACAO header.
const WRONG_ORIGIN: &str = "https://evil.example.test";
/// Fixed probe id, echoed by the middleware and the stub body.
const PROBE_ID: &str = "019aa1b2-c3d4-7e5f-8000-000000000002";

fn router_with_origin(origin: &str) -> axum::Router {
    let settings = Settings {
        web_origin: origin.to_string(),
        ..Settings::default()
    };
    let state = AppState::from_settings(&settings).expect("default settings build");
    build_router(state, Metrics::new())
}

fn request(method: Method, uri: &str, origin: Option<&str>, body: Body) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("x-request-id", PROBE_ID);
    if let Some(origin) = origin {
        builder = builder.header(header::ORIGIN, origin);
    }
    builder.body(body).unwrap()
}

async fn json_body(res: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .expect("read body");
    serde_json::from_slice(&bytes).expect("body is JSON")
}

fn assert_secure_headers(res: &axum::response::Response) {
    for (name, value) in SECURE_HEADERS {
        assert_eq!(
            res.headers().get(*name).and_then(|v| v.to_str().ok()),
            Some(*value),
            "response must carry secure header {name}: {value}"
        );
    }
}

fn assert_request_id(res: &axum::response::Response) {
    assert_eq!(
        res.headers()
            .get("x-request-id")
            .and_then(|v| v.to_str().ok()),
        Some(PROBE_ID),
        "response must echo the probe request id"
    );
}

#[tokio::test]
async fn preflight_has_request_id_but_no_secure_headers() {
    let router = router_with_origin(ORIGIN);
    let req = Request::builder()
        .method(Method::OPTIONS)
        .uri("/v1/jobs")
        .header("x-request-id", PROBE_ID)
        .header(header::ORIGIN, ORIGIN)
        .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
        .header(header::ACCESS_CONTROL_REQUEST_HEADERS, "authorization")
        .body(Body::empty())
        .unwrap();
    let res = router.oneshot(req).await.expect("oneshot preflight");
    assert_eq!(res.status(), StatusCode::OK, "preflight must be 200");
    assert_eq!(
        res.headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .and_then(|v| v.to_str().ok()),
        Some(ORIGIN),
        "preflight must allow the console origin"
    );
    assert_request_id(&res);
    // The preflight short-circuits in CORS, before the secure
    // headers layer: no HSTS/nosniff on a preflight.
    assert!(
        res.headers().get("strict-transport-security").is_none(),
        "preflight must NOT carry secure headers (CORS is outside them)"
    );
}

#[tokio::test]
async fn real_requests_carry_cors_secure_headers_and_request_id() {
    let router = router_with_origin(ORIGIN);
    let res = router
        .oneshot(request(
            Method::GET,
            "/healthz",
            Some(ORIGIN),
            Body::empty(),
        ))
        .await
        .expect("oneshot /healthz");
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .and_then(|v| v.to_str().ok()),
        Some(ORIGIN),
        "real request must carry ACAO"
    );
    assert_secure_headers(&res);
    assert_request_id(&res);
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .expect("read body");
    assert_eq!(body.as_ref(), b"ok");
}

#[tokio::test]
async fn stub_errors_carry_the_same_headers_as_problem_json() {
    let router = router_with_origin(ORIGIN);
    let res = router
        .oneshot(request(
            Method::GET,
            "/v1/jobs",
            Some(ORIGIN),
            Body::empty(),
        ))
        .await
        .expect("oneshot /v1/jobs");
    assert_eq!(res.status(), StatusCode::NOT_IMPLEMENTED);
    assert_eq!(
        res.headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/problem+json"),
        "stub body must be problem+json, not plain application/json"
    );
    assert_secure_headers(&res);
    assert_request_id(&res);
    let json = json_body(res).await;
    assert_eq!(json["code"], "PF-5004");
    assert_eq!(json["request_id"], PROBE_ID);
}

#[tokio::test]
async fn unknown_path_is_a_mapped_404_with_outer_headers() {
    let router = router_with_origin(ORIGIN);
    let res = router
        .oneshot(request(
            Method::GET,
            "/v1/nope",
            Some(ORIGIN),
            Body::empty(),
        ))
        .await
        .expect("oneshot /v1/nope");
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        res.headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/problem+json"),
        "the error mapper must rewrite the framework 404"
    );
    assert_eq!(
        res.headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .and_then(|v| v.to_str().ok()),
        Some(ORIGIN),
        "a mapped 404 must still carry ACAO (mapper is inside CORS)"
    );
    assert_secure_headers(&res);
    assert_request_id(&res);
    let json = json_body(res).await;
    assert_eq!(json["code"], "PF-2010", "{json}");
    assert_eq!(json["request_id"], PROBE_ID);
}

#[tokio::test]
async fn oversize_body_is_a_mapped_413_with_outer_headers() {
    let router = router_with_origin(ORIGIN);
    let big = vec![b'x'; MAX_JSON_BODY_BYTES + 1];
    let req = Request::builder()
        .method(Method::POST)
        .uri("/v1/auth/login")
        .header("x-request-id", PROBE_ID)
        .header(header::ORIGIN, ORIGIN)
        .header(header::CONTENT_TYPE, "application/json")
        .header(
            header::CONTENT_LENGTH,
            (MAX_JSON_BODY_BYTES + 1).to_string(),
        )
        .body(Body::from(big))
        .unwrap();
    let res = router.oneshot(req).await.expect("oneshot oversize login");
    assert_eq!(res.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(
        res.headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/problem+json"),
        "the error mapper must rewrite the limit layer's 413"
    );
    assert_eq!(
        res.headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .and_then(|v| v.to_str().ok()),
        Some(ORIGIN),
        "a mapped 413 must still carry ACAO (mapper is inside CORS)"
    );
    assert_secure_headers(&res);
    assert_request_id(&res);
    let json = json_body(res).await;
    assert_eq!(json["code"], "PF-2003", "{json}");
    assert_eq!(json["request_id"], PROBE_ID);
}

/// P0-069: the limit is global, not just on auth — an oversize
/// body to a second module (jobs) is rejected the same way.
#[tokio::test]
async fn oversize_body_rejected_on_jobs_too() {
    let router = router_with_origin(ORIGIN);
    let big = vec![b'x'; MAX_JSON_BODY_BYTES + 1];
    let req = Request::builder()
        .method(Method::POST)
        .uri("/v1/jobs")
        .header("x-request-id", PROBE_ID)
        .header(header::ORIGIN, ORIGIN)
        .header(header::CONTENT_TYPE, "application/json")
        .header(
            header::CONTENT_LENGTH,
            (MAX_JSON_BODY_BYTES + 1).to_string(),
        )
        .body(Body::from(big))
        .unwrap();
    let res = router.oneshot(req).await.expect("oneshot oversize job");
    assert_eq!(res.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let json = json_body(res).await;
    assert_eq!(json["code"], "PF-2003", "{json}");
    assert_eq!(json["request_id"], PROBE_ID);
}

#[tokio::test]
async fn uploads_are_exempt_from_the_body_limit() {
    let router = router_with_origin(ORIGIN);
    // 2 MiB — twice the JSON cap — to the uploads nest: the stub
    // answers 501, never 413. (Upload bytes never transit the api;
    // the nest carries no limit layer by design.)
    let big = vec![b'x'; 2 * MAX_JSON_BODY_BYTES];
    let req = Request::builder()
        .method(Method::POST)
        .uri("/v1/uploads")
        .header("x-request-id", PROBE_ID)
        .header(header::ORIGIN, ORIGIN)
        .header(header::CONTENT_TYPE, "application/json")
        .header(
            header::CONTENT_LENGTH,
            (2 * MAX_JSON_BODY_BYTES).to_string(),
        )
        .body(Body::from(big))
        .unwrap();
    let res = router.oneshot(req).await.expect("oneshot big upload");
    assert_eq!(
        res.status(),
        StatusCode::NOT_IMPLEMENTED,
        "uploads must be exempt from the body limit"
    );
    let json = json_body(res).await;
    assert_eq!(json["code"], "PF-5004");
}

#[tokio::test]
async fn cors_disabled_without_an_origin() {
    let router = router_with_origin("");
    let res = router
        .oneshot(request(
            Method::GET,
            "/healthz",
            Some(ORIGIN),
            Body::empty(),
        ))
        .await
        .expect("oneshot /healthz without origin");
    assert_eq!(res.status(), StatusCode::OK);
    assert!(
        res.headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .is_none(),
        "no origin configured: no ACAO header may ever be sent"
    );
}

#[tokio::test]
async fn wrong_origin_gets_no_acao() {
    let router = router_with_origin(ORIGIN);
    let res = router
        .oneshot(request(
            Method::GET,
            "/healthz",
            Some(WRONG_ORIGIN),
            Body::empty(),
        ))
        .await
        .expect("oneshot /healthz wrong origin");
    assert_eq!(res.status(), StatusCode::OK);
    assert!(
        res.headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .is_none(),
        "a non-console origin must not get ACAO"
    );
}
