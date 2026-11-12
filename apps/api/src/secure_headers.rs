//! Secure headers (P0-065).
//!
//! The header table the security doc requires
//! (`docs/05-QUALITY/03-security-testing.md`, "Headers &
//! transport"): CSP, HSTS, `X-Content-Type-Options`, and
//! `frame-ancestors none` for app routes — asserted by the route
//! test in `tests/middleware_chain.rs` against [`SECURE_HEADERS`].
//!
//! The values: the api serves JSON only, no HTML, so the CSP is
//! `default-src 'none'` plus `frame-ancestors 'none'` (the api
//! must never be framed); `X-Frame-Options: DENY` is the same
//! rule for older clients. HSTS is `max-age=31536000;
//! includeSubDomains` — harmless on plain-http localhost (browsers
//! ignore it there) and required behind the TLS-terminating Caddy
//! in prod. `X-Content-Type-Options: nosniff` stops MIME sniffing
//! of error bodies.
//!
//! Note for P0-067 (`/docs`, swagger-ui): a page that loads JS
//! needs its own CSP — the docs route will override this table,
//! not weaken it for the api.

use axum::{
    body::Body,
    http::{header, HeaderValue, Request},
    middleware::Next,
    response::Response,
};

/// The exact header table every api response carries.
/// (name, value) — the order test asserts this table verbatim.
pub const SECURE_HEADERS: &[(&str, &str)] = &[
    (
        "strict-transport-security",
        "max-age=31536000; includeSubDomains",
    ),
    ("x-content-type-options", "nosniff"),
    ("x-frame-options", "DENY"),
    (
        "content-security-policy",
        "default-src 'none'; frame-ancestors 'none'",
    ),
];

/// Axum middleware: stamp the secure-header table on every
/// response that passes through (success, stub, mapped error,
/// 404 — the layer sits outside the routes and the error mapper,
/// so nothing the api emits escapes it).
pub async fn secure_headers_middleware(req: Request<Body>, next: Next) -> Response {
    let mut res = next.run(req).await;
    for (name, value) in SECURE_HEADERS {
        if let (Ok(name), Ok(value)) = (
            header::HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(value),
        ) {
            res.headers_mut().insert(name, value);
        }
    }
    res
}
