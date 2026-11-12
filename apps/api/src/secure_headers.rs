//! Secure headers (P0-065, completed P0-102).
//!
//! The header table the security doc requires
//! (`docs/05-QUALITY/03-security-testing.md`, "Headers &
//! transport"): CSP, HSTS (prod), `X-Content-Type-Options`,
//! `Referrer-Policy`, and `frame-ancestors none` for app routes
//! — asserted by the route test in `tests/middleware_chain.rs`
//! against [`SECURE_HEADERS`] (plus the HSTS and docs-CSP legs
//! there).
//!
//! The values: the api serves JSON only, no HTML, so the CSP is
//! `default-src 'none'` plus `frame-ancestors 'none'` (the api
//! must never be framed); `X-Frame-Options: DENY` is the same
//! rule for older clients. `X-Content-Type-Options: nosniff`
//! stops MIME sniffing of error bodies. `Referrer-Policy:
//! no-referrer` — the api never needs to send a referrer
//! anywhere, so it sends none.
//!
//! HSTS is prod-only (P0-102): it is only honoured over TLS, so
//! stamping it on plain-http dev responses is meaningless noise.
//! The middleware reads the deployment environment from the
//! state (`Settings::hsts_enabled` — every environment except
//! dev, i.e. stage and prod behind the TLS-terminating Caddy)
//! and adds the HSTS header only there. Dev responses carry the
//! rest of the table but no HSTS — the order test asserts both
//! legs.
//!
//! CSP for api-served docs (P0-102): the Swagger UI at `/docs`
//! is an HTML page that loads JS/CSS from the same origin, so
//! the api's `default-src 'none'` would break it (the UI would
//! load with no scripts and no styles). [`docs_csp_middleware`]
//! replaces the CSP on `/docs` responses with [`DOCS_CSP`] —
//! same-origin scripts, styles (plus `'unsafe-inline'`, which
//! Swagger UI needs for its injected `<style>` blocks), images,
//! fonts, and `connect-src 'self'` so the UI can fetch
//! `/openapi.json` — while keeping `frame-ancestors 'none'`
//! (the docs page must never be framed either). It is layered
//! OUTSIDE the secure-headers middleware (see `build_router`)
//! so it stamps the CSP after the api table, on the way out —
//! an inner layer would be overwritten by the outer one. Every
//! other secure header still comes from the api table.

use axum::{
    body::Body,
    extract::State,
    http::{header, HeaderValue, Request},
    middleware::Next,
    response::Response,
};

/// The exact header table every api response carries.
/// (name, value) — the order test asserts this table verbatim.
/// HSTS is NOT in this table: it is prod-only and stamped
/// separately (see [`HSTS_HEADER`]).
pub const SECURE_HEADERS: &[(&str, &str)] = &[
    ("x-content-type-options", "nosniff"),
    ("x-frame-options", "DENY"),
    ("referrer-policy", "no-referrer"),
    (
        "content-security-policy",
        "default-src 'none'; frame-ancestors 'none'",
    ),
];

/// The HSTS header, kept out of [`SECURE_HEADERS`] because it is
/// prod-only (see the module docs): it is stamped only when
/// [`Settings::hsts_enabled`] says so.
pub const HSTS_HEADER: (&str, &str) = (
    "strict-transport-security",
    "max-age=31536000; includeSubDomains",
);

/// The CSP for the Swagger UI at `/docs` (P0-102). Same-origin
/// everything the UI needs — scripts, styles, images, fonts,
/// and the fetch of `/openapi.json` — and nothing else; the
/// frame-ancestors rule is the same as the api's.
pub const DOCS_CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self'; connect-src 'self'; frame-ancestors 'none'";

/// Axum middleware: stamp the secure-header table on every
/// response that passes through (success, stub, mapped error,
/// 404 — the layer sits outside the routes and the error mapper,
/// so nothing the api emits escapes it). HSTS is added only in
/// TLS-terminated environments (stage/prod) — see the module
/// docs. The middleware takes the state explicitly
/// (`from_fn_with_state`) because `from_fn` middleware cannot
/// extract the router state.
pub async fn secure_headers_middleware(
    State(state): State<crate::AppState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let mut res = next.run(req).await;
    stamp_headers(&mut res);
    if state.settings.hsts_enabled() {
        stamp_header(&mut res, HSTS_HEADER);
    }
    res
}

/// Axum middleware for the docs page: replace the CSP on `/docs`
/// responses with [`DOCS_CSP`] (the api table's `default-src
/// 'none'` would break the Swagger UI). Every other path passes
/// through untouched, and every other secure header is left
/// exactly as the inner middleware stamped it. Layer this
/// OUTSIDE `secure_headers_middleware` so it runs after it on
/// the way out.
pub async fn docs_csp_middleware(req: Request<Body>, next: Next) -> Response {
    let is_docs = req.uri().path().starts_with("/docs");
    let mut res = next.run(req).await;
    if is_docs {
        stamp_header(&mut res, ("content-security-policy", DOCS_CSP));
    }
    res
}

/// Stamp the base table on a response.
fn stamp_headers(res: &mut Response) {
    for (name, value) in SECURE_HEADERS {
        stamp_header(res, (*name, *value));
    }
}

/// Stamp one (name, value) pair on a response, replacing any
/// header of the same name already present (the docs CSP
/// override replaces the api CSP rather than adding a second
/// one — two CSP headers would be ANDed by the browser, which
/// would re-break the UI).
fn stamp_header(res: &mut Response, (name, value): (&str, &str)) {
    if let (Ok(name), Ok(value)) = (
        header::HeaderName::from_bytes(name.as_bytes()),
        HeaderValue::from_str(value),
    ) {
        res.headers_mut().insert(name, value);
    }
}
