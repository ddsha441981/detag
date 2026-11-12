//! HTTP metrics middleware (P0-028).
//!
//! Wraps every request on the public api router: the in-flight gauge
//! goes up on entry (RAII guard, down on exit), and on completion the
//! request counter and duration histogram are observed with the
//! route TEMPLATE (+ method + status). Templates only — raw ids never
//! reach a label (cardinality); unmatched paths report as `unmatched`
//! (same convention as the request-id span, P0-019). The metrics
//! instance comes from the `Extension` layered by `build_router`.

use axum::{body::Body, extract::MatchedPath, http::Request, middleware::Next, response::Response};
use detag_core::metrics::Metrics;
use std::time::Instant;

/// Axum middleware: count + time every request, track in-flight.
pub async fn http_metrics_middleware(req: Request<Body>, next: Next) -> Response {
    let metrics = req
        .extensions()
        .get::<Metrics>()
        .expect("http metrics Extension missing — build_router must layer it")
        .clone();
    let method = req.method().to_string();
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map(|m| m.as_str().to_string())
        .unwrap_or_else(|| "unmatched".to_string());
    let _inflight = metrics.http().start(&route, &method);
    let start = Instant::now();
    let response = next.run(req).await;
    let status = response.status().as_u16();
    metrics
        .http()
        .observe(&route, &method, status, start.elapsed());
    response
}
