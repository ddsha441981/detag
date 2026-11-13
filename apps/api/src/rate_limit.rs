//! Rate limiter skeleton (P0-103).
//!
//! The HTTP layer over the P0-048 primitives
//! (`detag_core::redis::ratelimit` — atomic Lua token buckets):
//! every `/v1` request spends one token from an IP bucket and
//! one from a route bucket; an empty bucket answers 429 +
//! problem+json (PF-3007) with `Retry-After` and the
//! `X-RateLimit-*` trio. Health/meta routes (`/healthz`,
//! `/readyz`, `/openapi.json`, `/docs`) are exempt — a load
//! balancer's probe must never trip a limit.
//!
//! Scopes (the spec's `{scope,id}` key — `pf:ratelimit:{scope}:{id}`):
//!
//! | scope | id | status |
//! | ----- | -- | ------ |
//! | `ip` | client IP | enforced (this ticket) |
//! | `route` | `METHOD:/v1/<segment>` | enforced (this ticket) |
//! | `user` | user id — or, pre-auth, the sha256 of the login email | enforced for login (P1-003, by the login service); the middleware still skips it (it cannot resolve an account from a request) |
//! | `api-key` | key prefix | Phase 1 (no auth yet — unresolvable) |
//!
//! The `api-key` scope exists in [`RateLimitScope`] so the key
//! shape is settled, but the middleware cannot resolve it until
//! Phase 1 auth lands — it is skipped, never enforced on a
//! guessed id. Enforcing a scope on a wrong id would rate-limit
//! the wrong principal. The `user` scope is the same shape with
//! one exception: the login service (P1-003) resolves it
//! pre-auth from the normalized email's hash, so the login
//! handler can spend from it directly.
//!
//! Identity resolution (IP): `X-Forwarded-For` first entry,
//! else the socket address (`ConnectInfo` — the binary wires
//! it in `main.rs`), else the single shared `"unknown"` bucket.
//! XFF is trusted because Caddy is the only ingress and it
//! sets the header — a client that talks to the api directly
//! can spoof it, but then the socket address is the fallback
//! and the spoof only buys a different bucket, not an
//! exemption. The route key uses the first two path segments
//! (`GET:/v1/jobs`), not the full path: ids in the path would
//! create one bucket per job (unbounded keys), and the route
//! template is what the limit protects. `MatchedPath` would be
//! the exact template, but it is not available to a
//! router-level layer — the two-segment key is the skeleton's
//! bounded approximation; Phase 1 can move the middleware to a
//! route layer and switch to it.
//!
//! Fail-open (documented decision): if Redis is down the
//! middleware logs a warning and lets the request through. A
//! rate limiter must not take the whole api down on a cache
//! outage — availability first, with the outage visible in the
//! logs (and in `/readyz`, which already reports the cache
//! component degraded). A `rate_limit_fail_open_total` counter
//! belongs to the observability pass; the warn is the skeleton's
//! signal. The threat model (P0-106) records the trade-off.
//!
//! Headers: `X-RateLimit-Limit` is the bucket's burst capacity,
//! `X-RateLimit-Remaining` the tokens left after this request,
//! `X-RateLimit-Reset` the unix time when one token is
//! available again (now, when one already is). On a denial the
//! same trio comes from the denying bucket, plus
//! `Retry-After: <seconds until one token>` (at least 1).
//! The allowed response carries the IP bucket's numbers — the
//! per-client limit is the one the caller can act on.

use crate::problem::ProblemResponse;
use crate::routes::extractors::RequestId;
use axum::{
    body::Body,
    extract::{ConnectInfo, State},
    http::{header, HeaderMap, HeaderName, HeaderValue, Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use detag_core::{
    config::Settings,
    error::{AppError, Profile},
    redis::ratelimit::{RateLimit, TokenBucket},
};
use std::{net::SocketAddr, time::SystemTime};

/// The rate-limit scopes (P0-103). The key segment each scope
/// contributes to `pf:ratelimit:{scope}:{id}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateLimitScope {
    /// Per client IP.
    Ip,
    /// Per authenticated user (Phase 1 — unresolvable now).
    User,
    /// Per API key prefix (Phase 1 — unresolvable now).
    ApiKey,
    /// Per route template (`METHOD:/v1/<segment>`).
    Route,
}

impl RateLimitScope {
    /// The key segment for this scope.
    pub fn key(self) -> &'static str {
        match self {
            RateLimitScope::Ip => "ip",
            RateLimitScope::User => "user",
            RateLimitScope::ApiKey => "api-key",
            RateLimitScope::Route => "route",
        }
    }
}

/// The two enforced buckets, built once from [`Settings`] and
/// carried in [`AppState`](crate::AppState). The per-plan tiers
/// are open question #17 (tuned in Phase 5); these are the
/// global skeleton defaults.
#[derive(Debug, Clone, Copy)]
pub struct RateLimitPolicy {
    /// Per-IP bucket: burst + refill from the settings.
    pub ip: TokenBucket,
    /// Per-route bucket: burst + refill from the settings.
    pub route: TokenBucket,
    /// Per-account login-attempt bucket (P1-003): burst +
    /// refill from the settings. Checked by the login service
    /// (not the middleware — the middleware cannot resolve an
    /// account from a request; the service keys it by the sha256
    /// of the normalized email, pre-auth).
    pub login_account: TokenBucket,
}

impl RateLimitPolicy {
    /// Build the policy from settings. Infallible in practice:
    /// `Settings::validate` already rejects a zero burst, and a
    /// refill of 0 is a legal fixed allowance — so
    /// `TokenBucket::new` cannot fail here; the `expect` marks
    /// that invariant rather than hiding an error path.
    pub fn from_settings(settings: &Settings) -> Self {
        let ip = TokenBucket::new(
            settings.rate_limit_ip_burst as f64,
            settings.rate_limit_ip_refill_per_min as f64 / 60.0,
        )
        .expect("settings validation guarantees a valid IP bucket");
        let route = TokenBucket::new(
            settings.rate_limit_route_burst as f64,
            settings.rate_limit_route_refill_per_min as f64 / 60.0,
        )
        .expect("settings validation guarantees a valid route bucket");
        let login_account = TokenBucket::new(
            settings.rate_limit_login_account_burst as f64,
            settings.rate_limit_login_account_refill_per_min as f64 / 60.0,
        )
        .expect("settings validation guarantees a valid login-account bucket");
        RateLimitPolicy {
            ip,
            route,
            login_account,
        }
    }

    /// The burst capacity of a bucket (for the `X-RateLimit-Limit`
    /// header). The capacity is stored as a float in the bucket;
    /// the settings value is the integer the operator set.
    pub fn ip_burst(&self) -> u64 {
        // The bucket was built from the integer burst, so this
        // round-trips exactly for any realistic limit.
        self.ip.capacity() as u64
    }

    /// The burst capacity of the route bucket.
    pub fn route_burst(&self) -> u64 {
        self.route.capacity() as u64
    }

    /// The burst capacity of the per-account login bucket.
    pub fn login_account_burst(&self) -> u64 {
        self.login_account.capacity() as u64
    }
}

/// The client IP for the `ip` scope: `X-Forwarded-For` first
/// entry, else the socket address, else `"unknown"` (one shared
/// bucket — an unidentifiable client is still bounded).
pub fn client_ip(req: &Request<Body>) -> String {
    let addr = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| *addr);
    client_ip_from(req.headers(), addr)
}

/// The same identity, from the pieces a handler has: the
/// request headers + the (optional) socket address. The login
/// handler (P1-004) uses it for the per-IP lockout counter —
/// the same identity the rate limiter uses, so the two agree
/// on who "the client" is. `"unknown"` means neither source
/// had an address (oneshot tests); the lockout skips the IP
/// counter for it (there is no IP identity to lock — see the
/// login service).
pub fn client_ip_from(headers: &HeaderMap, addr: Option<SocketAddr>) -> String {
    if let Some(xff) = headers.get(HeaderName::from_static("x-forwarded-for")) {
        if let Ok(xff) = xff.to_str() {
            if let Some(first) = xff.split(',').next() {
                let first = first.trim();
                if !first.is_empty() {
                    return first.to_string();
                }
            }
        }
    }
    if let Some(addr) = addr {
        return addr.ip().to_string();
    }
    "unknown".to_string()
}

/// The route key for the `route` scope: `METHOD:/v1/<segment>`
/// — the first two path segments, so ids in the path never
/// create a bucket each. The separator is a colon, not a space:
/// the Redis key builder rejects whitespace.
pub fn route_key(req: &Request<Body>) -> String {
    let path = req.uri().path();
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let scoped: Vec<&&str> = segments.iter().take(2).collect();
    let joined = scoped.iter().map(|s| **s).collect::<Vec<&str>>().join("/");
    format!("{}:/{joined}", req.method())
}

/// Axum middleware: spend one token from the IP bucket and one
/// from the route bucket for every `/v1` request. A denial
/// answers 429 + problem+json with `Retry-After`; an allowance
/// stamps the `X-RateLimit-*` trio and calls the next layer.
/// Non-`/v1` paths pass through untouched. Redis errors fail
/// open (see the module docs).
pub async fn rate_limit_middleware(
    State(state): State<crate::AppState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    if !req.uri().path().starts_with("/v1") {
        return next.run(req).await;
    }
    let policy = state.rate_limit;
    let request_id = req.extensions().get::<RequestId>().map(|id| id.0.clone());
    // The IP check first: it is the per-client signal, and its
    // numbers are the ones an allowed response carries.
    let ip = client_ip(&req);
    let ip_check = policy
        .ip
        .check(&state.redis, RateLimitScope::Ip.key(), &ip, 1.0)
        .await;
    let ip_result = match ip_check {
        Ok(result) => result,
        Err(err) => {
            tracing::warn!(scope = "ip", error = %err, "rate limiter failing open: redis unavailable");
            return next.run(req).await;
        }
    };
    if !ip_result.allowed {
        return denied_response(ip_result, policy.ip_burst(), request_id.as_deref());
    }
    let route = route_key(&req);
    let route_check = policy
        .route
        .check(&state.redis, RateLimitScope::Route.key(), &route, 1.0)
        .await;
    let route_result = match route_check {
        Ok(result) => result,
        Err(err) => {
            tracing::warn!(scope = "route", error = %err, "rate limiter failing open: redis unavailable");
            return next.run(req).await;
        }
    };
    if !route_result.allowed {
        return denied_response(route_result, policy.route_burst(), request_id.as_deref());
    }
    let mut res = next.run(req).await;
    stamp_ratelimit_headers(&mut res, ip_result, policy.ip_burst());
    res
}

/// The 429 answer: the PF-3007 problem body (rendered exactly
/// like the error mapper renders it) plus `Retry-After` and the
/// denying bucket's `X-RateLimit-*` trio. Shared with the login
/// handler (P1-003), whose per-account denial is rendered the
/// same way.
pub(crate) fn denied_response(result: RateLimit, limit: u64, request_id: Option<&str>) -> Response {
    let problem = AppError::RateLimited.to_problem(Profile::Production, request_id, None);
    let mut res = ProblemResponse(problem).into_response();
    debug_assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
    stamp_ratelimit_headers(&mut res, result, limit);
    let retry_after = result.reset_in.as_secs().max(1);
    if let Ok(value) = HeaderValue::from_str(&retry_after.to_string()) {
        res.headers_mut().insert(header::RETRY_AFTER, value);
    }
    res
}

/// Stamp the `X-RateLimit-*` trio from one check: limit is the
/// bucket's burst, remaining the tokens left, reset the unix
/// time when one token is available again.
fn stamp_ratelimit_headers(res: &mut Response, result: RateLimit, limit: u64) {
    let reset = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() + result.reset_in.as_secs())
        .unwrap_or(0);
    let headers = res.headers_mut();
    if let Ok(value) = HeaderValue::from_str(&limit.to_string()) {
        headers.insert("x-ratelimit-limit", value);
    }
    if let Ok(value) = HeaderValue::from_str(&result.remaining.to_string()) {
        headers.insert("x-ratelimit-remaining", value);
    }
    if let Ok(value) = HeaderValue::from_str(&reset.to_string()) {
        headers.insert("x-ratelimit-reset", value);
    }
}
