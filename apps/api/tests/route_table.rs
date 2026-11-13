//! P0-064: the route-table test.
//!
//! Two checks, both against the router `build_router` builds
//! (state + middleware included, so the probes travel the real
//! path a request takes):
//!
//! 1. The table itself has no duplicate (method, path) pairs
//!    and has the 65 entries the API spec lists (63 from the
//!    spec + `POST /v1/auth/resend-verification`, added by
//!    P1-002 — the spec names no resend route — + `POST
//!    /v1/auth/confirm-email-change`, added by P1-006 — the
//!    spec names no confirm route either).
//! 2. Every table entry resolves to the 501 stub (never 404):
//!    the route exists and its handler is the shared stub.
//!    A duplicate or overlapping route would already have
//!    panicked while building the router (axum), so building
//!    it IS the duplicate check; the probes are the presence
//!    check. Negative controls (paths that must NOT exist)
//!    assert 404, proving the probes are not vacuously passing.
//!    Exception: routes whose handler is real (P1-001
//!    `/v1/auth/register`; P1-002 `/v1/auth/verify-email` +
//!    `/v1/auth/resend-verification`; P1-003 `/v1/auth/login`)
//!    answer with their real behavior for the empty-body probe
//!    — 422 PF-2002, since `ValidatedJson` rejects the empty
//!    body — and the probe asserts that instead of 501. P1-005
//!    `GET` + `PATCH /v1/me` answer 401 PF-1001 for the probe
//!    (no credential — the auth extractor rejects before the
//!    handler runs).
//!
//! The probes send a fixed `x-request-id` and assert the 501
//! problem body echoes it — that proves the whole chain at
//! once: middleware assigns the id, inserts it into the
//! extensions, the stub renders it into the body.

use std::collections::HashSet;

use axum::http::{Method, Request, StatusCode};
use detag_api::routes::ROUTE_TABLE;
use detag_api::{build_router, AppState};
use detag_core::config::Settings;
use detag_core::metrics::Metrics;
use tower::ServiceExt;
use uuid::Uuid;

/// The probe id: a fixed UUIDv7-shaped string, echoed by the
/// middleware and the stub body alike.
const PROBE_ID: &str = "019aa1b2-c3d4-7e5f-8000-000000000001";

/// Fill `{id}` params with the probe id (the stub never reads
/// them, but the route must match a concrete path).
fn probe_path(path: &str) -> String {
    path.replace("{id}", PROBE_ID)
}

fn test_router() -> axum::Router {
    let state = AppState::from_settings(&Settings::default()).expect("default settings build");
    build_router(state, Metrics::new())
}

#[test]
fn route_table_has_no_duplicates() {
    let mut seen = HashSet::new();
    for (method, path) in ROUTE_TABLE {
        assert!(
            seen.insert((*method, *path)),
            "duplicate route in ROUTE_TABLE: {method} {path}"
        );
    }
    assert_eq!(
        ROUTE_TABLE.len(),
        65,
        "ROUTE_TABLE must list the spec's 65 routes (auth 18 + uploads 4 + jobs 9 + billing 12 + admin 20 + meta 2)"
    );
}

#[tokio::test]
async fn every_table_entry_resolves_to_the_stub() {
    let router = test_router();
    // The probes send 62 /v1 requests in one burst through the
    // rate limiter (P0-103). They use a per-run XFF address so
    // they never share the oneshot "unknown" bucket with the
    // other test binaries running in parallel — the aggregate
    // "unknown" traffic tripped the 120-burst IP bucket once
    // (P1-003's login tests added ~30 parallel requests).
    // The middleware treats XFF as an opaque bucket id, so a
    // UUID suffix is a valid key.
    let xff = format!("203.0.113.{}", Uuid::now_v7());
    for (method, path) in ROUTE_TABLE {
        // The two meta routes are real handlers, not stubs:
        // /healthz answers 200 "ok", /readyz answers 200-or-503
        // with a readiness body (200-or-503 depends on whether
        // the dev Postgres/Redis are up at that moment).
        if *path == "/healthz" {
            let req = Request::builder()
                .method(Method::GET)
                .uri("/healthz")
                .body(axum::body::Body::empty())
                .unwrap();
            let res = router.clone().oneshot(req).await.expect("oneshot /healthz");
            assert_eq!(res.status(), StatusCode::OK, "/healthz must be 200");
            let body = axum::body::to_bytes(res.into_body(), usize::MAX)
                .await
                .expect("read body");
            assert_eq!(body.as_ref(), b"ok");
            continue;
        }
        if *path == "/readyz" {
            let req = Request::builder()
                .method(Method::GET)
                .uri("/readyz")
                .body(axum::body::Body::empty())
                .unwrap();
            let res = router.clone().oneshot(req).await.expect("oneshot /readyz");
            assert!(
                res.status() == StatusCode::OK || res.status() == StatusCode::SERVICE_UNAVAILABLE,
                "/readyz must be 200 or 503, got {}",
                res.status()
            );
            continue;
        }
        // Real handlers (not stubs) answer with their real
        // behavior for the empty-body probe. P1-001 register +
        // P1-002 verify-email / resend-verification + P1-003
        // login + P1-006 confirm-email-change: the empty body
        // fails ValidatedJson → 422 PF-2002 with the probe id
        // echoed (the route exists; each handler's own test
        // covers its success paths).
        if *method == "POST"
            && matches!(
                *path,
                "/v1/auth/register"
                    | "/v1/auth/verify-email"
                    | "/v1/auth/resend-verification"
                    | "/v1/auth/confirm-email-change"
                    | "/v1/auth/login"
            )
        {
            let req = Request::builder()
                .method(Method::POST)
                .uri(*path)
                .header("x-request-id", PROBE_ID)
                .header("x-forwarded-for", xff.as_str())
                .body(axum::body::Body::empty())
                .unwrap();
            let res = router
                .clone()
                .oneshot(req)
                .await
                .unwrap_or_else(|err| panic!("oneshot POST {path} failed: {err:?}"));
            assert_eq!(
                res.status(),
                StatusCode::UNPROCESSABLE_ENTITY,
                "POST {path} with an empty body must be 422, got {}",
                res.status()
            );
            let body = axum::body::to_bytes(res.into_body(), usize::MAX)
                .await
                .expect("read body");
            let json: serde_json::Value =
                serde_json::from_slice(&body).expect("real-handler body is JSON");
            assert_eq!(json["code"], "PF-2002", "POST {path}: {json}");
            assert_eq!(json["request_id"], PROBE_ID, "POST {path}: {json}");
            continue;
        }
        // P1-005 GET + PATCH /v1/me: real handlers behind the
        // auth extractor — the probe sends no credential, so the
        // extractor rejects with 401 PF-1001 (not 501) and echoes
        // the probe id (the route exists; each handler's own test
        // covers its success paths).
        if (*method == "GET" || *method == "PATCH") && *path == "/v1/me" {
            let req = Request::builder()
                .method(Method::from_bytes(method.as_bytes()).expect("method parses"))
                .uri(*path)
                .header("x-request-id", PROBE_ID)
                .header("x-forwarded-for", xff.as_str())
                .body(axum::body::Body::empty())
                .unwrap();
            let res = router
                .clone()
                .oneshot(req)
                .await
                .unwrap_or_else(|err| panic!("oneshot {method} {path} failed: {err:?}"));
            assert_eq!(
                res.status(),
                StatusCode::UNAUTHORIZED,
                "{method} {path} without a credential must be 401, got {}",
                res.status()
            );
            let body = axum::body::to_bytes(res.into_body(), usize::MAX)
                .await
                .expect("read body");
            let json: serde_json::Value =
                serde_json::from_slice(&body).expect("auth-rejection body is JSON");
            assert_eq!(json["code"], "PF-1001", "{method} {path}: {json}");
            assert_eq!(json["request_id"], PROBE_ID, "{method} {path}: {json}");
            continue;
        }
        let uri = probe_path(path);
        let req = Request::builder()
            .method(Method::from_bytes(method.as_bytes()).expect("method parses"))
            .uri(&uri)
            .header("x-request-id", PROBE_ID)
            .header("x-forwarded-for", xff.as_str())
            .body(axum::body::Body::empty())
            .unwrap();
        let res = router
            .clone()
            .oneshot(req)
            .await
            .unwrap_or_else(|err| panic!("oneshot {method} {path} failed: {err:?}"));
        assert_eq!(
            res.status(),
            StatusCode::NOT_IMPLEMENTED,
            "{method} {path} must resolve to the 501 stub, got {}",
            res.status()
        );
        // The stub body is the problem envelope with the probe id.
        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .expect("read body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("stub body is JSON");
        assert_eq!(json["code"], "PF-5004", "{method} {path}: {json}");
        assert_eq!(json["status"], 501);
        assert_eq!(json["request_id"], PROBE_ID, "{method} {path}: {json}");
    }
}

#[tokio::test]
async fn unknown_paths_are_404() {
    let router = test_router();
    // Same per-run XFF as the probe test (see above): the /v1
    // 404s travel the rate limiter too.
    let xff = format!("203.0.113.{}", Uuid::now_v7());
    for (method, path) in [
        ("GET", "/v1/nope"),
        (
            "POST",
            "/v1/jobs/019aa1b2-c3d4-7e5f-8000-000000000001/frobnicate",
        ),
        ("GET", "/v1/admin"),
        ("DELETE", "/v1/auth"),
        ("GET", "/v2/jobs"),
    ] {
        let req = Request::builder()
            .method(Method::from_bytes(method.as_bytes()).expect("method parses"))
            .uri(path)
            .header("x-forwarded-for", xff.as_str())
            .body(axum::body::Body::empty())
            .unwrap();
        let res = router
            .clone()
            .oneshot(req)
            .await
            .unwrap_or_else(|err| panic!("oneshot {method} {path} failed: {err:?}"));
        assert_eq!(
            res.status(),
            StatusCode::NOT_FOUND,
            "{method} {path} must be 404, got {}",
            res.status()
        );
    }
}
