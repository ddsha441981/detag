//! Request-id middleware (P0-019).
//!
//! Every request gets a request id. The incoming `x-request-id` header
//! is echoed when it parses as a UUID (any version — clients and
//! proxies send v4 ids too); otherwise a fresh UUIDv7 is generated
//! (`Uuid::now_v7()`). The id is echoed back on the response as
//! `x-request-id`, and a tracing span `http_request` carries
//! `request_id`, `route` (the matched path template, or `unmatched` for
//! 404s so spans stay low-cardinality) and `method` for the whole
//! downstream call.
//!
//! Log sampling (P0-019, full structured access log P0-072): the
//! per-request `request completed` event is emitted always for
//! errors (status >= 400) and slow requests (> 250 ms), and for a
//! deterministic 1% of successes — the decision is a hash of the
//! request id, so the same id always decides the same way. The span
//! itself is created for every request regardless of sampling.
//!
//! The event carries `status` + `elapsed_ms` as its own fields, and
//! `sampled`: true only when the line exists because the 1% sample
//! picked this request (a fast success) — false for errors and slow
//! requests, which always log. `request_id`, `route` and `method`
//! travel on the `http_request` span and appear on the same line
//! (the JSON formatter carries span fields — see the telemetry
//! module's `json_logs_parse_and_carry_span_fields` test), so the
//! line as a whole is the access log: method, route, status,
//! latency, request id, and why the line exists. No query string
//! is ever logged (it can carry tokens — log hygiene, P0-032).

use axum::{
    body::Body,
    extract::MatchedPath,
    http::{HeaderValue, Request},
    middleware::Next,
    response::Response,
};
use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    time::{Duration, Instant},
};
use tracing::Instrument;
use uuid::Uuid;

/// The request-id header name, request and response.
pub const REQUEST_ID_HEADER: &str = "x-request-id";

/// The request id for this request, as assigned by
/// [`request_id_middleware`]. The middleware inserts it into the
/// request extensions, so handlers read it with
/// `Extension<RequestId>` (see `routes/extractors.rs`) — the same
/// id the response header echoes and the problem bodies carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestId(pub String);

/// Slow-request threshold: slower than this, the access event is always
/// emitted (per NFR-1 / P0-072).
pub const SLOW_THRESHOLD: Duration = Duration::from_millis(250);

/// Axum middleware: assign/echo the request id, span the request,
/// sample the access-log event.
pub async fn request_id_middleware(mut req: Request<Body>, next: Next) -> Response {
    let id = extract_or_generate(&req);
    // Handlers read the id from here (Extension<RequestId>).
    req.extensions_mut().insert(RequestId(id.clone()));
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map(|m| m.as_str().to_string())
        .unwrap_or_else(|| "unmatched".to_string());
    let method = req.method().to_string();
    let span = tracing::info_span!(
        "http_request",
        request_id = %id,
        route = %route,
        method = %method,
    );
    async move {
        let start = Instant::now();
        let mut res = next.run(req).await;
        let status = res.status().as_u16();
        if let Ok(value) = HeaderValue::from_str(&id) {
            res.headers_mut().insert(REQUEST_ID_HEADER, value);
        }
        let elapsed = start.elapsed();
        if should_log(status, elapsed, &id) {
            tracing::info!(
                status = status,
                elapsed_ms = elapsed.as_millis() as u64,
                sampled = is_sampled(status, elapsed),
                "request completed"
            );
        }
        res
    }
    .instrument(span)
    .await
}

/// The incoming id if it parses as a UUID, else a fresh UUIDv7.
fn extract_or_generate(req: &Request<Body>) -> String {
    if let Some(value) = req.headers().get(REQUEST_ID_HEADER) {
        if let Ok(text) = value.to_str() {
            if Uuid::parse_str(text).is_ok() {
                return text.to_string();
            }
        }
    }
    Uuid::now_v7().to_string()
}

/// Sampling decision for the access-log event: errors always, slow
/// requests always, successes a deterministic 1% by request-id hash.
pub fn should_log(status: u16, elapsed: Duration, request_id: &str) -> bool {
    if status >= 400 || elapsed > SLOW_THRESHOLD {
        return true;
    }
    let mut hasher = DefaultHasher::new();
    request_id.hash(&mut hasher);
    hasher.finish().is_multiple_of(100)
}

/// True when this request's access event exists only because the 1%
/// sample picked it (a fast success) — false for errors and slow
/// requests, which always log. The `sampled` field on the event.
pub fn is_sampled(status: u16, elapsed: Duration) -> bool {
    status < 400 && elapsed <= SLOW_THRESHOLD
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_always_log() {
        assert!(should_log(400, Duration::ZERO, "any-id"));
        assert!(should_log(404, Duration::ZERO, "any-id"));
        assert!(should_log(500, Duration::ZERO, "any-id"));
    }

    #[test]
    fn slow_successes_always_log() {
        assert!(should_log(200, Duration::from_millis(251), "any-id"));
        assert!(should_log(200, Duration::from_secs(2), "any-id"));
    }

    #[test]
    fn fast_success_is_deterministic_per_id() {
        let fast = Duration::from_millis(10);
        for id in ["a", "b", "c", "01JREQUESTID"] {
            assert_eq!(should_log(200, fast, id), should_log(200, fast, id));
        }
    }

    #[test]
    fn sampled_flag_is_true_only_for_sampled_successes() {
        let fast = Duration::from_millis(10);
        // A fast success is the only case the event exists because
        // of sampling.
        assert!(is_sampled(200, fast));
        assert!(is_sampled(301, fast));
        // Errors and slow requests always log, so they are not
        // "sampled" lines.
        assert!(!is_sampled(400, fast));
        assert!(!is_sampled(500, fast));
        assert!(!is_sampled(200, Duration::from_millis(251)));
    }

    /// 1% sampling over 10k generated UUIDv7s: the hash is deterministic
    /// (DefaultHasher has fixed keys), so this band is stable, not flaky.
    #[test]
    fn success_sampling_is_about_one_percent() {
        let fast = Duration::from_millis(10);
        let sampled = (0..10_000)
            .filter(|_| {
                let id = Uuid::now_v7().to_string();
                should_log(200, fast, &id)
            })
            .count();
        assert!(
            (50..=200).contains(&sampled),
            "expected ~100 sampled of 10k, got {sampled}"
        );
    }
}
