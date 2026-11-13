//! P0-107: the security smoke suite — one named gate over the
//! five Phase 0 hardening ACs, driven through the real router
//! (`TestApp` oneshot, the same harness as the endpoint tests):
//!
//! 1. Headers present (P0-102): the secure-header table +
//!    request id on a real response, HSTS absent in dev.
//! 2. Rate limit trips at threshold (P0-103): burst 5 admits 5,
//!    the 6th is 429 PF-3007 with Retry-After.
//! 3. Oversized body rejected (P0-104): 65 KiB on the 64 KiB auth
//!    class is 413 PF-2003; the same body on jobs (1 MiB class)
//!    is admitted — the cap is per class, not global.
//! 4. Method-not-allowed: DELETE on the GET-only `/healthz` is
//!    405 with an `Allow` header naming GET.
//! 5. No swagger in prod profile (P0-102): `/docs` and `/docs/`
//!    are 404 with `PF_ENVIRONMENT=prod`, while `/healthz` still
//!    answers 200 with HSTS; in dev `/docs` 303-redirects to
//!    `/docs/`.
//!
//! The deeper per-feature proofs live in `middleware_chain.rs`
//! (headers, body caps), `rate_limit.rs` (buckets, the race)
//! and `validation.rs`. This file is the gate: if any of the
//! five regresses, the suite name in the CI output says so.
//! It deliberately duplicates a few assertions from those files
//! — a smoke suite that only re-runs other suites proves
//! nothing; it must own its five checks.

mod common;

use axum::{
    body::Body,
    http::{header, Method, Request, StatusCode},
};
use common::TestApp;
use detag_api::secure_headers::{HSTS_HEADER, SECURE_HEADERS};
use detag_core::config::Settings;
use uuid::Uuid;

/// The console origin used by the tests (never a real origin).
const ORIGIN: &str = "https://console.example.test";
/// Fixed probe id, echoed by the request-id middleware.
const REQUEST_ID: &str = "123e4567-e89b-42d3-a456-426614174107";

/// A fresh IP per test: the XFF value is an opaque bucket id, so
/// a uuid guarantees a bucket no earlier run has touched.
fn fresh_ip() -> String {
    Uuid::now_v7().to_string()
}

/// Settings with the test's limits. Refills are per minute;
/// pass 0 for a never-refilling bucket (deterministic trips).
/// The console origin is the test ORIGIN (the default web_origin
/// is the dev console, not this one — without it, CORS never
/// stamps ACAO and the headers test fails for the wrong reason).
fn test_settings(
    ip_burst: u64,
    ip_refill_per_min: u64,
    route_burst: u64,
    route_refill_per_min: u64,
) -> Settings {
    Settings {
        web_origin: ORIGIN.to_string(),
        rate_limit_ip_burst: ip_burst,
        rate_limit_ip_refill_per_min: ip_refill_per_min,
        rate_limit_route_burst: route_burst,
        rate_limit_route_refill_per_min: route_refill_per_min,
        ..Settings::default()
    }
}

/// Default settings with the test console origin.
fn base_settings() -> Settings {
    Settings {
        web_origin: ORIGIN.to_string(),
        ..Settings::default()
    }
}

fn request(method: Method, uri: &str, xff: &str, body: Body) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", xff)
        .header(header::ORIGIN, ORIGIN)
        .body(body)
        .unwrap()
}

fn hdr<'a>(res: &'a common::TestResponse, name: &str) -> Option<&'a str> {
    res.headers.get(name).and_then(|v| v.to_str().ok())
}

#[tokio::test]
async fn headers_present_on_real_response() {
    let app = TestApp::spawn_with(base_settings()).await;
    let res = app
        .request(request(Method::GET, "/healthz", &fresh_ip(), Body::empty()))
        .await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(res.text(), "ok");
    for (name, value) in SECURE_HEADERS {
        assert_eq!(
            hdr(&res, name),
            Some(*value),
            "response must carry secure header {name}: {value}"
        );
    }
    assert_eq!(hdr(&res, "x-request-id"), Some(REQUEST_ID));
    assert_eq!(
        hdr(&res, header::ACCESS_CONTROL_ALLOW_ORIGIN.as_str()),
        Some(ORIGIN),
        "real request must carry ACAO for the console origin"
    );
    assert!(
        hdr(&res, HSTS_HEADER.0).is_none(),
        "dev must NOT carry HSTS (P0-102: prod-only)"
    );
}

#[tokio::test]
async fn rate_limit_trips_at_threshold() {
    let app = TestApp::spawn_with(test_settings(5, 0, 1000, 60_000)).await;
    let ip = fresh_ip();
    // Five requests: admitted (the stub answers 501 — the status
    // is the stub's business; admission is what matters).
    for i in 1..=5 {
        let res = app
            .request(request(Method::GET, "/v1/jobs", &ip, Body::empty()))
            .await;
        assert_eq!(
            res.status,
            StatusCode::NOT_IMPLEMENTED,
            "request {i} admitted"
        );
        assert_eq!(hdr(&res, "x-ratelimit-limit"), Some("5"));
        let expected = (5 - i).to_string();
        assert_eq!(
            hdr(&res, "x-ratelimit-remaining"),
            Some(expected.as_str()),
            "remaining must count down"
        );
        assert!(
            hdr(&res, "retry-after").is_none(),
            "admitted: no Retry-After"
        );
    }
    // Sixth: denied — 429 + PF-3007 problem+json + Retry-After.
    let res = app
        .request(request(Method::GET, "/v1/jobs", &ip, Body::empty()))
        .await;
    assert_eq!(res.status, StatusCode::TOO_MANY_REQUESTS);
    let json = res.json();
    assert_eq!(json["code"], "PF-3007", "{json}");
    assert_eq!(json["request_id"], REQUEST_ID);
    assert!(
        hdr(&res, "retry-after").is_some_and(|v| v.parse::<u64>().is_ok_and(|s| s >= 1)),
        "denial must carry Retry-After >= 1s"
    );
}

#[tokio::test]
async fn oversized_body_rejected() {
    let app = TestApp::spawn_with(base_settings()).await;
    let ip = fresh_ip();
    // 65 KiB: over the 64 KiB auth class, under the 1 MiB jobs class.
    let body = vec![b'x'; 65 * 1024];
    let req = Request::builder()
        .method(Method::POST)
        .uri("/v1/auth/login")
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", &ip)
        .header(header::ORIGIN, ORIGIN)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CONTENT_LENGTH, body.len().to_string())
        .body(Body::from(body.clone()))
        .unwrap();
    let res = app.request(req).await;
    assert_eq!(res.status, StatusCode::PAYLOAD_TOO_LARGE);
    let json = res.json();
    assert_eq!(json["code"], "PF-2003", "{json}");
    assert_eq!(
        json["title"], "payload_too_large",
        "413 must be the mapped problem body, not the bare limit response"
    );

    // Same body on jobs: admitted (501 is the stub, not a cap).
    let req = Request::builder()
        .method(Method::POST)
        .uri("/v1/jobs")
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", &ip)
        .header(header::ORIGIN, ORIGIN)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CONTENT_LENGTH, body.len().to_string())
        .body(Body::from(body))
        .unwrap();
    let res = app.request(req).await;
    assert_eq!(
        res.status,
        StatusCode::NOT_IMPLEMENTED,
        "65 KiB is under the jobs class cap (1 MiB)"
    );
}

#[tokio::test]
async fn method_not_allowed() {
    let app = TestApp::spawn_with(base_settings()).await;
    let res = app
        .request(request(
            Method::DELETE,
            "/healthz",
            &fresh_ip(),
            Body::empty(),
        ))
        .await;
    assert_eq!(res.status, StatusCode::METHOD_NOT_ALLOWED);
    let allow = hdr(&res, header::ALLOW.as_str()).unwrap_or_default();
    assert!(
        allow.contains("GET"),
        "405 must name the allowed method (Allow: {allow})"
    );
    // The outer layers still stamp the response even though the
    // router itself made it.
    assert_eq!(hdr(&res, "x-request-id"), Some(REQUEST_ID));
    for (name, value) in SECURE_HEADERS {
        assert_eq!(
            hdr(&res, name),
            Some(*value),
            "405 must still carry secure header {name}"
        );
    }
    // NOTE: the body is axum's bare 405, NOT problem+json — the
    // error mapper rewrites 400/404/413/415/422 only (P0-065).
    // Whether 405 belongs in the envelope is a Phase 1
    // error-envelope question, not a smoke-suite change.
    assert_ne!(
        hdr(&res, header::CONTENT_TYPE.as_str()),
        Some("application/problem+json"),
        "405 is currently bare — if this ever becomes problem+json, update the note above"
    );
}

#[tokio::test]
async fn no_swagger_in_prod_profile() {
    // Prod: docs are not registered at all.
    let prod = TestApp::spawn_with(Settings {
        environment: "prod".to_string(),
        web_origin: ORIGIN.to_string(),
        ..Settings::default()
    })
    .await;
    let ip = fresh_ip();
    for uri in ["/docs", "/docs/"] {
        let res = prod
            .request(request(Method::GET, uri, &ip, Body::empty()))
            .await;
        assert_eq!(
            res.status,
            StatusCode::NOT_FOUND,
            "{uri} must be 404 in prod"
        );
        let json = res.json();
        assert_eq!(json["code"], "PF-2010", "{json}"); // mapped 404
    }
    // Prod still serves: healthz answers, with HSTS.
    let res = prod
        .request(request(Method::GET, "/healthz", &ip, Body::empty()))
        .await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(hdr(&res, HSTS_HEADER.0), Some(HSTS_HEADER.1));

    // Dev contrast: /docs exists (Swagger UI 303-redirects to
    // /docs/), so the prod 404 is the profile, not a missing route.
    let dev = TestApp::spawn_with(base_settings()).await;
    let res = dev
        .request(request(Method::GET, "/docs", &fresh_ip(), Body::empty()))
        .await;
    assert_eq!(
        res.status,
        StatusCode::SEE_OTHER,
        "dev /docs must 303 to /docs/"
    );
    assert!(
        hdr(&res, header::LOCATION.as_str()).is_some_and(|loc| loc.ends_with("/docs/")),
        "dev /docs must redirect to /docs/"
    );
}
