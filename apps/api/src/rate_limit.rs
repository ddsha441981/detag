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
//! | `api-key` | key id (UUID) | enforced for key callers (P1-037, by the extractor — see below) |
//! | `login-ip` | sha256 of the client IP | enforced for login (P1-020, by the login service) |
//! | `register-email` | sha256 of the normalized email | enforced for register (P1-020, by the register service) |
//! | `reset-account` | sha256 of the normalized email | P1-025 wires it (no reset endpoint yet — the bucket exists, nothing spends from it) |
//!
//! ONE BUCKET, ONE KEY (P1-020): the Lua script takes the
//! capacity as an argument, so two buckets that share a Redis
//! key corrupt each other — the script would refill both at
//! different rates and admit against the wrong capacity. Every
//! scope therefore has its own key segment, and a service
//! bucket never shares a segment with the middleware buckets
//! or with another service bucket. A new bucket gets a new
//! segment, never a reused one — this table is the registry.
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
//! # Per-key rate limits (P1-037, FR-A-06)
//!
//! The extractor enforces the `api-key` scope — the middleware
//! still cannot, because only the extractor holds the verified
//! key. After the scope check passes (and before `note_key_use`,
//! so a denied request is not a use), the extractor spends one
//! token from the key's tier bucket ([`check_api_key_rate_limit`]):
//! a denial answers 429 + problem+json (PF-3007) with
//! `Retry-After` and the KEY bucket's `X-RateLimit-*` trio
//! (via [`denied_response`]); the middleware never overwrites
//! those headers — it stamps its IP numbers only when the trio
//! is absent. An ALLOWED key response still carries the IP
//! trio (the middleware's rule, unchanged): the key bucket's
//! remaining is visible on the denial, not on every 200 —
//! the extractor cannot stamp response headers, and a
//! post-extractor header layer is a bigger change than this
//! ticket's "429 headers" spec asks for.
//!
//! Tiers ([`ApiKeyTier`]): the row's `rate_limit_tier`
//! resolves to `default` (NULL, `"default"`, or any unknown
//! name — a typo degrades to the smallest bucket, never to a
//! denial and never to a bigger bucket) or `high` (`"high"`).
//! The default tier is 60 burst / 60 per min from config
//! (`PF_API_KEY_RATE_LIMIT_BURST` /
//! `PF_API_KEY_RATE_LIMIT_REFILL_PER_MIN`) — deliberately
//! lower than the user tier (the per-IP bucket, 120/120), so
//! a runaway key trips before it can crowd out user traffic
//! on the shared route bucket. The `high` tier is 600/600
//! (`PF_API_KEY_RATE_LIMIT_HIGH_*`) for trusted integrations;
//! the tier is set out of band (direct row update — no
//! endpoint sets it yet). The bucket id is the key's UUID,
//! not its display prefix: two keys can share a 4-char prefix
//! (24 bits), and a shared bucket would rate-limit the wrong
//! principal — the P0-103 table said "key prefix" when the
//! scope was still a skeleton, and this ticket corrects it.
//!
//! Abuse log: every denial increments a per-key counter
//! (`pf:abuse:api-key-ratelimit:{key_id}`, window
//! `PF_API_KEY_RATE_LIMIT_ABUSE_WINDOW_SECS`, default 3600 s).
//! When the count REACHES
//! `PF_API_KEY_RATE_LIMIT_ABUSE_THRESHOLD` (default 5), one
//! `api_key.rate_limit_tripped` audit row is written (actor
//! the key's owner, target the key id, `after` carrying the
//! key prefix + tier + burst + trips-in-window — never the
//! secret or its hash, NFR-42). Later denials in the same
//! window write nothing — a sustained attack writes at most
//! one row per key per window. The audit write is
//! best-effort: a failed insert logs loudly but the denial
//! stands (the mailer drain's P1-031 shape).
//!
//! Fail-open (the P0-103 decision, same as the middleware):
//! a Redis error on the bucket check is a warn + the request
//! proceeds — a cache outage must not take auth down. The
//! scope check already ran, so the fail-open only loses the
//! rate limit, never the authorization.
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
//! per-client limit is the one the caller can act on. The
//! middleware stamps the trio only when the handler has not
//! already (P1-020): a service-level denial carries the
//! DENYING bucket's numbers, and the middleware must never
//! overwrite them with the IP bucket's.

use crate::problem::ProblemResponse;
use crate::routes::extractors::RequestId;
use axum::{
    body::Body,
    extract::{ConnectInfo, State},
    http::{header, HeaderMap, HeaderName, HeaderValue, Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use deadpool_redis::redis::cmd;
use detag_core::{
    api_keys::{
        API_KEY_RATE_LIMIT_TIER_DEFAULT, API_KEY_RATE_LIMIT_TIER_HIGH,
        API_KEY_RATE_LIMIT_TRIPPED_ACTION,
    },
    config::Settings,
    error::{AppError, Profile},
    redis::{
        keys::api_key_rate_limit_abuse_key,
        ratelimit::{RateLimit, RateLimitError, TokenBucket},
    },
    repos::{
        api_keys::ApiKey,
        audit_logs::{insert as insert_audit, NewAuditLog},
    },
};
use serde_json::json;
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
    /// Per client IP, for login attempts (P1-020): the same
    /// identity as `Ip`, but a SEPARATE bucket — the login
    /// bucket's capacity (20/10) differs from the middleware's
    /// (120/120), and two buckets must never share a Redis key
    /// (see the module docs). Keyed by the sha256 of the IP,
    /// like the lockout's IP counter.
    LoginIp,
    /// Per email, for registration attempts (P1-020): keyed by
    /// the sha256 of the normalized email, pre-auth.
    RegisterEmail,
    /// Per account, for password-reset attempts (P1-020): keyed
    /// by the sha256 of the normalized email, pre-auth. The
    /// reset flow (P1-025) wires the check; nothing spends from
    /// it until then.
    ResetAccount,
}

impl RateLimitScope {
    /// The key segment for this scope.
    pub fn key(self) -> &'static str {
        match self {
            RateLimitScope::Ip => "ip",
            RateLimitScope::User => "user",
            RateLimitScope::ApiKey => "api-key",
            RateLimitScope::Route => "route",
            RateLimitScope::LoginIp => "login-ip",
            RateLimitScope::RegisterEmail => "register-email",
            RateLimitScope::ResetAccount => "reset-account",
        }
    }
}

/// The enforced buckets, built once from [`Settings`] and
/// carried in [`AppState`](crate::AppState). The per-plan tiers
/// are open question #17 (tuned in Phase 5); these are the
/// global skeleton defaults. The middleware spends from `ip`
/// and `route`; the services spend from the rest (the
/// middleware cannot resolve an account from a request, so
/// service buckets are checked by the service that knows the
/// identity). Every bucket has its own key segment — see the
/// module docs ("ONE BUCKET, ONE KEY").
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
    /// Per-IP login-attempt bucket (P1-020): burst + refill
    /// from the settings. Checked by the login service after
    /// the account bucket, keyed by the sha256 of the client
    /// IP. Bounds credential-stuffing runs from one IP against
    /// many accounts; the account bucket bounds the reverse
    /// (one account from many IPs).
    pub login_ip: TokenBucket,
    /// Per-email registration bucket (P1-020): burst + refill
    /// from the settings. Checked by the register service
    /// before the Argon2id hash, keyed by the sha256 of the
    /// normalized email, pre-auth. Duplicate attempts spend it
    /// too — a duplicate registration IS a registration
    /// attempt.
    pub register_email: TokenBucket,
    /// Per-account password-reset bucket (P1-020): burst +
    /// refill from the settings. The reset flow (P1-025) wires
    /// the check; nothing spends from it until then.
    pub reset_account: TokenBucket,
    /// Per-key default-tier bucket (P1-037): burst + refill
    /// from the settings. Checked by the extractor (not the
    /// middleware — the middleware cannot resolve a key from
    /// a request; the extractor holds the verified key).
    pub api_key_default: TokenBucket,
    /// Per-key `high`-tier bucket (P1-037).
    pub api_key_high: TokenBucket,
    /// Denials for one key inside the abuse window before the
    /// trip is written to the abuse audit log (P1-037).
    pub api_key_abuse_threshold: u64,
    /// The abuse window, in seconds (P1-037).
    pub api_key_abuse_window_secs: u64,
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
        let login_ip = TokenBucket::new(
            settings.rate_limit_login_ip_burst as f64,
            settings.rate_limit_login_ip_refill_per_min as f64 / 60.0,
        )
        .expect("settings validation guarantees a valid login-IP bucket");
        let register_email = TokenBucket::new(
            settings.rate_limit_register_email_burst as f64,
            settings.rate_limit_register_email_refill_per_min as f64 / 60.0,
        )
        .expect("settings validation guarantees a valid register-email bucket");
        let reset_account = TokenBucket::new(
            settings.rate_limit_reset_account_burst as f64,
            settings.rate_limit_reset_account_refill_per_min as f64 / 60.0,
        )
        .expect("settings validation guarantees a valid reset-account bucket");
        let api_key_default = TokenBucket::new(
            settings.api_key_rate_limit_burst as f64,
            settings.api_key_rate_limit_refill_per_min as f64 / 60.0,
        )
        .expect("settings validation guarantees a valid default key bucket");
        let api_key_high = TokenBucket::new(
            settings.api_key_rate_limit_high_burst as f64,
            settings.api_key_rate_limit_high_refill_per_min as f64 / 60.0,
        )
        .expect("settings validation guarantees a valid high key bucket");
        RateLimitPolicy {
            ip,
            route,
            login_account,
            login_ip,
            register_email,
            reset_account,
            api_key_default,
            api_key_high,
            api_key_abuse_threshold: settings.api_key_rate_limit_abuse_threshold,
            api_key_abuse_window_secs: settings.api_key_rate_limit_abuse_window_secs,
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

    /// The burst capacity of the per-IP login bucket.
    pub fn login_ip_burst(&self) -> u64 {
        self.login_ip.capacity() as u64
    }

    /// The burst capacity of the per-email registration bucket.
    pub fn register_email_burst(&self) -> u64 {
        self.register_email.capacity() as u64
    }

    /// The burst capacity of the per-account reset bucket.
    pub fn reset_account_burst(&self) -> u64 {
        self.reset_account.capacity() as u64
    }

    /// The bucket for a key's tier (P1-037).
    pub fn api_key_bucket(&self, tier: ApiKeyTier) -> TokenBucket {
        match tier {
            ApiKeyTier::Default => self.api_key_default,
            ApiKeyTier::High => self.api_key_high,
        }
    }

    /// The burst capacity of the per-key default-tier bucket.
    pub fn api_key_default_burst(&self) -> u64 {
        self.api_key_default.capacity() as u64
    }

    /// The burst capacity of the per-key `high`-tier bucket.
    pub fn api_key_high_burst(&self) -> u64 {
        self.api_key_high.capacity() as u64
    }
}

/// The per-key rate-limit tier a row resolves to (P1-037,
/// FR-A-06). The tier is a property of the KEY (its row),
/// not of the user — two keys of one user may sit in
/// different tiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiKeyTier {
    /// NULL, `"default"`, or any unknown name on the row.
    Default,
    /// `"high"` on the row — the trusted-integration tier.
    High,
}

impl ApiKeyTier {
    /// The tier name as stored on the row / written to logs
    /// and the audit row.
    pub fn as_str(self) -> &'static str {
        match self {
            ApiKeyTier::Default => API_KEY_RATE_LIMIT_TIER_DEFAULT,
            ApiKeyTier::High => API_KEY_RATE_LIMIT_TIER_HIGH,
        }
    }
}

/// Resolve a row's `rate_limit_tier` to a tier (P1-037):
/// `None` and `"default"` are the default tier, `"high"` is
/// the high tier, and anything else falls back to default —
/// an unknown name never grants a bigger bucket and never
/// denies service (the enforcement site logs the fallback,
/// so a typo'd tier is visible, not silent).
pub fn resolve_api_key_tier(tier: Option<&str>) -> ApiKeyTier {
    match tier {
        Some(name) if name == API_KEY_RATE_LIMIT_TIER_HIGH => ApiKeyTier::High,
        _ => ApiKeyTier::Default,
    }
}

/// The answer to one per-key rate-limit check (P1-037): the
/// bucket's verdict plus the burst the 429 carries as its
/// `X-RateLimit-Limit`. (The tier name itself is consumed
/// inside the check — for the bucket choice and the audit row
/// — and is not returned: the caller renders the denial from
/// these two numbers alone.)
#[derive(Debug, Clone, Copy)]
pub(crate) struct ApiKeyRateLimitDecision {
    /// The bucket's verdict (allowed / remaining / reset-in).
    pub result: RateLimit,
    /// The tier's burst — the `X-RateLimit-Limit` of a denial.
    pub limit: u64,
}

/// Spend one token from a verified key's tier bucket (P1-037).
/// The bucket is the `api-key` scope, keyed by the key's UUID
/// — never by its display prefix (two keys can share a 4-char
/// prefix; a shared bucket would rate-limit the wrong
/// principal). On a denial the abuse trip is recorded (see
/// below) before returning — the caller renders the 429 from
/// the decision. A Redis error is `Err`: the caller fails
/// OPEN (warn + proceed) — a cache outage must not take auth
/// down (the P0-103 decision).
pub(crate) async fn check_api_key_rate_limit(
    state: &crate::AppState,
    key: &ApiKey,
) -> Result<ApiKeyRateLimitDecision, RateLimitError> {
    let tier = resolve_api_key_tier(key.rate_limit_tier.as_deref());
    if let Some(name) = key.rate_limit_tier.as_deref() {
        if name != API_KEY_RATE_LIMIT_TIER_DEFAULT && name != API_KEY_RATE_LIMIT_TIER_HIGH {
            tracing::warn!(
                key_id = %key.id,
                tier = name,
                "api-key rate limit: unknown tier, falling back to default"
            );
        }
    }
    let bucket = state.rate_limit.api_key_bucket(tier);
    let result = bucket
        .check(
            &state.redis,
            RateLimitScope::ApiKey.key(),
            &key.id.to_string(),
            1.0,
        )
        .await?;
    if !result.allowed {
        record_api_key_rate_limit_trip(state, key, tier).await;
    }
    Ok(ApiKeyRateLimitDecision {
        result,
        limit: bucket.capacity() as u64,
    })
}

/// Record one per-key rate-limit denial (P1-037): increment
/// the key's abuse counter and, when the count REACHES the
/// threshold, write exactly one `api_key.rate_limit_tripped`
/// audit row for this key in this window. Later denials in
/// the same window write nothing — a sustained attack must
/// not flood the audit table one row per request. The counter
/// is `pf:abuse:api-key-ratelimit:{key_id}` with the abuse
/// window as its TTL (started on the first trip — INCR is
/// atomic, so exactly one caller sees `1` and sets the
/// expiry). Best-effort throughout: a Redis or DB failure
/// logs a warn/error and the denial stands — enforcement
/// never depends on the log write (the mailer drain's P1-031
/// shape).
async fn record_api_key_rate_limit_trip(state: &crate::AppState, key: &ApiKey, tier: ApiKeyTier) {
    let threshold = state.rate_limit.api_key_abuse_threshold;
    let window = state.rate_limit.api_key_abuse_window_secs;
    let counter_key = api_key_rate_limit_abuse_key(&key.id);
    let mut conn = match state.redis.get().await {
        Ok(conn) => conn,
        Err(err) => {
            tracing::warn!(
                error = %err,
                key_id = %key.id,
                "api-key rate limit: abuse counter unavailable, skipping abuse audit"
            );
            return;
        }
    };
    let trips: i64 = match cmd("INCR").arg(&counter_key).query_async(&mut conn).await {
        Ok(n) => n,
        Err(err) => {
            tracing::warn!(
                error = %err,
                key_id = %key.id,
                "api-key rate limit: abuse counter INCR failed, skipping abuse audit"
            );
            return;
        }
    };
    if trips == 1 {
        // First trip in the window starts it. A failed
        // EXPIRE only shortens the window's accuracy — the
        // counter keeps counting, the limit still holds.
        if let Err(err) = cmd("EXPIRE")
            .arg(&counter_key)
            .arg(window as i64)
            .query_async::<()>(&mut conn)
            .await
        {
            tracing::warn!(
                error = %err,
                key_id = %key.id,
                "api-key rate limit: abuse counter EXPIRE failed"
            );
        }
    }
    if trips as u64 != threshold {
        return;
    }
    let bucket = state.rate_limit.api_key_bucket(tier);
    let reason = format!(
        "api key rate limit: {} tier, {} denials in the last {}s",
        tier.as_str(),
        trips,
        window
    );
    tracing::warn!(
        key_id = %key.id,
        tier = tier.as_str(),
        trips_in_window = trips,
        "api-key rate limit tripped — abuse audit written"
    );
    if let Err(err) = insert_audit(
        &state.pool,
        &NewAuditLog {
            actor_user_id: Some(key.user_id),
            actor_client: Some("api_key".to_string()),
            action: API_KEY_RATE_LIMIT_TRIPPED_ACTION.to_string(),
            target_type: "api_key".to_string(),
            target_id: Some(key.id.to_string()),
            before: None,
            after: Some(json!({
                "key_prefix": key.prefix,
                "tier": tier.as_str(),
                "burst": bucket.capacity() as u64,
                "trips_in_window": trips,
                "window_secs": window,
            })),
            reason: Some(reason),
        },
    )
    .await
    {
        tracing::error!(
            error = %err,
            key_id = %key.id,
            "api-key rate limit: abuse audit insert failed"
        );
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
    // A downstream handler may have already stamped the trio:
    // a service-level denial (login account / login-IP /
    // register-email bucket, P1-003/P1-020) carries the
    // DENYING bucket's numbers, and those must reach the
    // caller intact — the middleware's IP numbers are the
    // wrong bucket. Stamp only when the handler left the
    // headers absent (every non-denial response).
    if !res.headers().contains_key("x-ratelimit-limit") {
        stamp_ratelimit_headers(&mut res, ip_result, policy.ip_burst());
    }
    res
}

/// The 429 answer: the PF-3007 problem body (rendered exactly
/// like the error mapper renders it) plus `Retry-After` and the
/// denying bucket's `X-RateLimit-*` trio. Shared with the login
/// and register handlers (P1-003, P1-020), whose per-account /
/// per-IP / per-email denials are rendered the same way.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_resolution_is_default_unless_high() {
        // P1-037: NULL and "default" are the default tier;
        // "high" is the high tier; anything else falls back
        // to default — never to a bigger bucket, never to a
        // denial (the enforcement site logs the fallback).
        assert_eq!(resolve_api_key_tier(None), ApiKeyTier::Default);
        assert_eq!(resolve_api_key_tier(Some("default")), ApiKeyTier::Default);
        assert_eq!(resolve_api_key_tier(Some("high")), ApiKeyTier::High);
        assert_eq!(resolve_api_key_tier(Some("bogus")), ApiKeyTier::Default);
        assert_eq!(resolve_api_key_tier(Some("")), ApiKeyTier::Default);
        assert_eq!(
            resolve_api_key_tier(Some("HIGH")),
            ApiKeyTier::Default,
            "tier names are case-sensitive — HIGH is not high"
        );
        assert_eq!(ApiKeyTier::Default.as_str(), "default");
        assert_eq!(ApiKeyTier::High.as_str(), "high");
    }

    #[test]
    fn policy_builds_both_key_tiers_from_settings() {
        // P1-037: the default tier is lower than the user
        // tier (IP 120/120); the high tier matches the route
        // tier (600/600); the abuse policy rides along.
        let settings = Settings::default();
        let policy = RateLimitPolicy::from_settings(&settings);
        assert_eq!(policy.api_key_default_burst(), 60);
        assert_eq!(policy.api_key_high_burst(), 600);
        assert_eq!(policy.api_key_bucket(ApiKeyTier::Default).capacity(), 60.0);
        assert_eq!(policy.api_key_bucket(ApiKeyTier::High).capacity(), 600.0);
        assert!(policy.api_key_default_burst() < policy.ip_burst());
        assert!(policy.api_key_default_burst() < policy.route_burst());
        assert_eq!(policy.api_key_abuse_threshold, 5);
        assert_eq!(policy.api_key_abuse_window_secs, 3600);

        // A custom settings object moves both buckets (the
        // integration tests drive burst=2 this way).
        let settings = Settings {
            api_key_rate_limit_burst: 2,
            api_key_rate_limit_refill_per_min: 0,
            api_key_rate_limit_high_burst: 5,
            api_key_rate_limit_high_refill_per_min: 0,
            api_key_rate_limit_abuse_threshold: 2,
            api_key_rate_limit_abuse_window_secs: 120,
            ..Settings::default()
        };
        let policy = RateLimitPolicy::from_settings(&settings);
        assert_eq!(policy.api_key_default_burst(), 2);
        assert_eq!(policy.api_key_high_burst(), 5);
        assert_eq!(policy.api_key_abuse_threshold, 2);
        assert_eq!(policy.api_key_abuse_window_secs, 120);
    }
}
