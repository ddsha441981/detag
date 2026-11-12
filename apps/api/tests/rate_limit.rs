//! P0-103: the rate limiter skeleton, end to end.
//!
//! The middleware spends one token from the IP bucket and one
//! from the route bucket per `/v1` request (atomic Lua scripts,
//! P0-048). The tests below drive the real router against the
//! dev Redis — a fresh bucket per test (a uuid `X-Forwarded-For`
//! for the IP scope, a uuid path for the route scope), so no
//! test can see another run's bucket. The AC is the race test:
//! 20 concurrent requests against a burst-5 bucket admit
//! exactly 5 — the Lua script is the atomicity, this test is
//! the proof the middleware actually uses it.

mod common;

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use common::TestApp;
use detag_api::rate_limit::{client_ip, route_key};
use detag_core::config::Settings;
use tower::ServiceExt;
use uuid::Uuid;

/// Fixed request id (a valid UUID, so the middleware echoes it).
const REQUEST_ID: &str = "123e4567-e89b-42d3-a456-426614174001";

/// A fresh IP per test: the XFF value is an opaque bucket id, so
/// a uuid guarantees a bucket no earlier run has touched.
fn fresh_ip() -> String {
    Uuid::now_v7().to_string()
}

/// Settings with the test's limits. Refills are per minute;
/// pass 0 for a never-refilling bucket (deterministic trips).
fn test_settings(
    ip_burst: u64,
    ip_refill_per_min: u64,
    route_burst: u64,
    route_refill_per_min: u64,
) -> Settings {
    Settings {
        rate_limit_ip_burst: ip_burst,
        rate_limit_ip_refill_per_min: ip_refill_per_min,
        rate_limit_route_burst: route_burst,
        rate_limit_route_refill_per_min: route_refill_per_min,
        ..Settings::default()
    }
}

fn request(method: Method, uri: &str, xff: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", xff)
        .body(Body::empty())
        .unwrap()
}

fn hdr<'a>(res: &'a common::TestResponse, name: &str) -> Option<&'a str> {
    res.headers.get(name).and_then(|v| v.to_str().ok())
}

#[tokio::test]
async fn ip_bucket_trips_at_threshold() {
    let app = TestApp::spawn_with(test_settings(5, 60, 1000, 60_000)).await;
    let ip = fresh_ip();
    // Five requests: admitted (the stub answers 501 — the
    // status is the stub's business; admission is what matters).
    for i in 1..=5 {
        let res = app.request(request(Method::GET, "/v1/jobs", &ip)).await;
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
    let res = app.request(request(Method::GET, "/v1/jobs", &ip)).await;
    assert_eq!(res.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(hdr(&res, "x-ratelimit-limit"), Some("5"));
    assert_eq!(hdr(&res, "x-ratelimit-remaining"), Some("0"));
    let retry_after = hdr(&res, "retry-after").expect("denial must carry Retry-After");
    let retry_after: u64 = retry_after.parse().expect("Retry-After must be seconds");
    assert!(retry_after >= 1, "Retry-After must be at least 1 s");
    let json = res.json();
    assert_eq!(json["code"], "PF-3007", "{json}");
    assert_eq!(json["request_id"], REQUEST_ID, "{json}");
    // A different IP has its own bucket: admitted again.
    let res = app
        .request(request(Method::GET, "/v1/jobs", &fresh_ip()))
        .await;
    assert_eq!(res.status, StatusCode::NOT_IMPLEMENTED);
}

/// P0-103 AC: no over-admission under race. Twenty concurrent
/// requests against a burst-5, never-refilling bucket admit
/// exactly 5 — the refill is 0 so timing cannot admit a sixth,
/// and the Lua script is atomic, so two requests cannot both
/// see the last token.
#[tokio::test]
async fn no_over_admission_under_race() {
    let app = TestApp::spawn_with(test_settings(5, 0, 1000, 60_000)).await;
    let ip = fresh_ip();
    let mut tasks = Vec::new();
    for _ in 0..20 {
        let router = app.router.clone();
        let req = request(Method::GET, "/v1/jobs", &ip);
        tasks.push(tokio::spawn(async move {
            router.oneshot(req).await.expect("oneshot must answer")
        }));
    }
    let mut admitted = 0;
    let mut denied = 0;
    for task in tasks {
        let res = task.await.expect("request task must finish");
        match res.status() {
            StatusCode::TOO_MANY_REQUESTS => denied += 1,
            _ => admitted += 1,
        }
    }
    assert_eq!(admitted, 5, "exactly the burst is admitted, got {admitted}");
    assert_eq!(denied, 15, "the rest are denied, got {denied}");
}

/// The route bucket is independent of the IP bucket: four
/// requests from four different IPs to one route trip the
/// route bucket (burst 3) while every IP bucket is fresh.
#[tokio::test]
async fn route_bucket_trips_independently() {
    let app = TestApp::spawn_with(test_settings(1000, 60, 3, 0)).await;
    // A uuid path, so the route key is a bucket no earlier run
    // has touched (the route key is METHOD + first two segments).
    let path = format!("/v1/rl-probe-{}", Uuid::now_v7());
    for i in 1..=3 {
        let res = app.request(request(Method::GET, &path, &fresh_ip())).await;
        assert_eq!(res.status, StatusCode::NOT_FOUND, "request {i} admitted");
        assert_eq!(hdr(&res, "x-ratelimit-limit"), Some("1000"));
    }
    let res = app.request(request(Method::GET, &path, &fresh_ip())).await;
    assert_eq!(res.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(hdr(&res, "x-ratelimit-limit"), Some("3"));
    assert_eq!(hdr(&res, "x-ratelimit-remaining"), Some("0"));
    let json = res.json();
    assert_eq!(json["code"], "PF-3007", "{json}");
}

/// Health/meta routes are exempt: even a burst-2 limit does not
/// touch `/healthz` — a load balancer's probe must never trip.
#[tokio::test]
async fn health_routes_are_exempt() {
    let app = TestApp::spawn_with(test_settings(2, 0, 2, 0)).await;
    let ip = fresh_ip();
    for _ in 0..5 {
        let res = app.request(request(Method::GET, "/healthz", &ip)).await;
        assert_eq!(res.status, StatusCode::OK);
        assert!(hdr(&res, "x-ratelimit-limit").is_none());
    }
    let res = app.request(request(Method::GET, "/readyz", &ip)).await;
    assert_ne!(res.status, StatusCode::TOO_MANY_REQUESTS);
}

/// Pure helpers: XFF wins over the socket address, the first
/// entry wins over the rest, and the route key is the method +
/// the first two path segments (ids never create a bucket).
#[test]
fn client_ip_prefers_xff_first_entry() {
    let req = Request::builder()
        .method(Method::GET)
        .uri("/v1/jobs")
        .header("x-forwarded-for", "203.0.113.7, 198.51.100.2")
        .body(Body::empty())
        .unwrap();
    assert_eq!(client_ip(&req), "203.0.113.7");
}

#[test]
fn client_ip_unknown_without_headers() {
    let req = Request::builder()
        .method(Method::GET)
        .uri("/v1/jobs")
        .body(Body::empty())
        .unwrap();
    assert_eq!(client_ip(&req), "unknown");
}

#[test]
fn route_key_uses_first_two_segments() {
    let req = Request::builder()
        .method(Method::GET)
        .uri("/v1/jobs/019aa1b2-c3d4-7e5f-8000-000000000002")
        .body(Body::empty())
        .unwrap();
    assert_eq!(route_key(&req), "GET:/v1/jobs");
    let req = Request::builder()
        .method(Method::POST)
        .uri("/v1/uploads")
        .body(Body::empty())
        .unwrap();
    assert_eq!(route_key(&req), "POST:/v1/uploads");
}
