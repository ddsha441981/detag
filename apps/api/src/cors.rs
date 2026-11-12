//! CORS (P0-065): the console is the only browser client.
//!
//! One exact origin from `PF_WEB_ORIGIN` (`Settings::web_origin`).
//! Empty disables CORS entirely — no `Access-Control-Allow-Origin`
//! is ever sent and preflights are rejected (fail-closed; the
//! console origin is set per environment). Exactly one origin on
//! purpose: an allowlist that grows by accident is how CORS stops
//! meaning anything. Credentials are allowed because the console
//! is a first-party app (a cookie-based refresh transport later
//! would silently fail without it); with a single exact origin
//! this is safe — there is no wildcard to abuse.
//!
//! The origin is registered as an `AllowOrigin::list` (not
//! `exact`): tower-http's `exact` emits the configured origin on
//! EVERY response, even to a request from another origin (the
//! browser would reject it, but the header would still be there
//! to confuse an audit). The list form emits `ACAO` only when
//! the request `Origin` matches — a wrong origin gets no ACAO
//! header at all, which is what the order test asserts.
//!
//! Allowed request headers are the ones the api spec uses:
//! `Content-Type`, `Authorization`, `X-Request-Id`, and
//! `Idempotency-Key`. Exposed response headers are the ones a
//! browser client needs to read: `X-Request-Id` and the
//! `X-RateLimit-*` trio. Preflight results are cached 10 minutes.

use axum::http::{header, HeaderValue, Method};
use std::time::Duration;
use tower_http::cors::{AllowOrigin, CorsLayer};

/// Preflight cache lifetime (seconds).
pub const CORS_MAX_AGE_SECS: u64 = 600;

/// Build the CORS layer for one origin. An empty or unparsable
/// origin disables CORS (fail-closed — see the module docs; config
/// validation already rejects a non-URL origin at boot, so the
/// unparsable path is belt-and-braces for direct callers).
pub fn cors_layer(web_origin: &str) -> CorsLayer {
    let mut layer = CorsLayer::new()
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([
            header::CONTENT_TYPE,
            header::AUTHORIZATION,
            header::HeaderName::from_static("x-request-id"),
            header::HeaderName::from_static("idempotency-key"),
        ])
        .expose_headers([
            header::HeaderName::from_static("x-request-id"),
            header::HeaderName::from_static("x-ratelimit-limit"),
            header::HeaderName::from_static("x-ratelimit-remaining"),
            header::HeaderName::from_static("x-ratelimit-reset"),
        ])
        .allow_credentials(true)
        .max_age(Duration::from_secs(CORS_MAX_AGE_SECS));
    if !web_origin.trim().is_empty() {
        if let Ok(origin) = HeaderValue::from_str(web_origin) {
            layer = layer.allow_origin(AllowOrigin::list([origin]));
        }
    }
    layer
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_origin_builds_a_layer() {
        // No panic, no origin: the layer exists and simply allows
        // nothing (proven at the integration level in
        // tests/middleware_chain.rs).
        let _layer = cors_layer("");
    }

    #[test]
    fn unparsable_origin_is_disabled_not_a_panic() {
        let _layer = cors_layer("not an origin \n with spaces");
    }
}
