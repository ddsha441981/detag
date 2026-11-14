//! Auth context extractor (P0-070, FR-A-02 skeleton; P1-005
//! fills in the Bearer verification, P1-016 completes it).
//!
//! [`AuthContext`] is the one type every handler takes to learn
//! who the caller is: which credential was presented (Bearer JWT,
//! API key, or web session cookie), and the caller's [`Role`].
//! It is an axum `FromRequestParts` extractor, so a handler
//! declares it as a parameter and the framework runs it before
//! the handler body.
//!
//! # Phase 1 status — Bearer verified, cookie final, API keys live
//!
//! A `Bearer <jwt>` is verified (HS256 signature + expiry +
//! not-before, leeway 0) against `PF_JWT_SECRET` and the `sub`
//! claim is loaded as a user: a verified context carries the
//! [`User`] (`verified: true`, the real [`Role`] from the user
//! row — re-read from the database on EVERY request, so a role
//! change takes effect immediately and the token's embedded
//! role is never trusted for admin routes, P1-022). The
//! rejections:
//! - no credential → 401 `PF-1001`;
//! - a Bearer token that is not a valid JWT → 401 `PF-1010`;
//!   an expired one → 401 `PF-1011`; a not-yet-valid one
//!   (`nbf` in the future) → 401 `PF-1010` (immature, not
//!   expired, P1-022);
//! - a valid JWT for an unknown user id → 401 `PF-1001` (the token
//!   is well-formed but the account is gone);
//! - a Bearer API key → verified by hash lookup (P1-034, below);
//! - a session cookie, alone → 401 `PF-1001` — and that is FINAL,
//!   not a placeholder (P1-016): the `detag_refresh` cookie holds
//!   a REFRESH token, which is not an access credential. The web
//!   client holds its access token in memory and sends it as
//!   `Bearer` on API calls; the cookie exists only for
//!   `POST /v1/auth/refresh` (P1-014). A genuine, live refresh
//!   token presented as the sole credential still 401s — proven
//!   by `tests/dual_auth.rs`, so a stolen refresh token cannot
//!   walk straight into the API as if it were an access token.
//!
//! # API keys (P1-034, FR-A-06)
//!
//! A `Bearer pf_live_*` key verifies by sha256 hash lookup
//! against the `api_keys` table (live keys only — a revoked
//! key reads exactly like an unknown one, 401 `PF-1001`,
//! P1-036's kill path). The key's owner must be `active`
//! (ADR-0024): suspend revokes sessions but not keys, and a
//! key has no expiry, so without this a suspended user's key
//! would work forever — the JWT path's "status is not checked
//! here" rule (P1-008) holds only because an access token dies
//! in ≤ 15 min. Then the scope check: the route (from
//! `MatchedPath`) names the scope it needs
//! ([`required_scope_for`]) — jobs writes need `jobs:write`,
//! jobs reads `jobs:read`, webhook endpoints
//! `webhooks:manage` — and a key without it is 403 `PF-1002`.
//! A route the table does not name rejects EVERY key with
//! 403 `PF-1002` (fail closed — P1-040's ACL matrix owns the
//! full table; routes that do not take `AuthContext` yet run
//! no extractor at all, so their key handling lands with
//! their handler ticket). Keys never reach admin routes — the
//! table names none, and both admin guards reject a
//! key-carrying context outright. A verified key use
//! re-stamps `last_used_at`, throttled to once per 300 s
//! (`API_KEY_LAST_USED_THROTTLE_SECS`); a denied request is
//! not a use. The key's `rate_limit_tier` rides the context
//! and is ENFORCED here (P1-037, FR-A-06): after the scope
//! check, one token is spent from the key's tier bucket
//! (`default` 60 burst / 60 per min from config — lower than
//! the user IP tier 120/120 — or `high` 600/600 for keys
//! whose row says so); an empty bucket answers 429 `PF-3007`
//! with `Retry-After` and the key bucket's `X-RateLimit-*`
//! trio, and repeated trips write one
//! `api_key.rate_limit_tripped` audit row per key per hour.
//! A Redis error fails open (warn logged) — the scope check
//! already ran, so only the rate limit degrades.
//!
//! P1-016 owns the rest: the precedence (Bearer wins, and stays
//! winning — see below) and the CSRF posture documentation. A
//! suspended/deleted user is NOT rejected here: suspension
//! revokes sessions at suspend time (P1-008), and the stateless
//! access token's remaining lifetime (≤ 15 min) is that
//! ticket's design space, not this extractor's.
//!
//! # Revocation (P1-021)
//!
//! A verified token can still be dead, two ways — both checked
//! here, after the JWT verifies and the user loads:
//! 1. **The epoch.** `users.revoked_before` is the instant
//!    before which every access token of the user is dead
//!    (bumped by a password change — P1-018's service, same tx
//!    as the session revocation). A token whose `iat` is
//!    earlier than it is 401 `PF-1010`. The column rides the
//!    user row already loaded, so this check costs zero extra
//!    queries. A token minted in the same second AFTER the bump
//!    is denied too (its `iat` is earlier than the bump
//!    instant) and self-heals: the client refreshes — its
//!    refresh token is still valid — and gets a live token.
//!    The comparison is strict (`iat < revoked_before`) and
//!    errs toward denial by construction (see
//!    `users::set_revoked_before`).
//! 2. **The jti denylist.** One Redis key per revoked token
//!    (`pf:revocation:jti:{jti}`, TTL = the token's remaining
//!    life). A token whose `jti` is present is 401 `PF-1010`.
//!    A token without a `jti` (pre-P1-013 legacy) skips the
//!    check — the epoch still covers it via `iat`. A Redis
//!    error fails OPEN (warn logged): a cache outage must not
//!    take auth down — the epoch check is DB-backed and still
//!    runs, so a password change still kills tokens during an
//!    outage; only the per-token denylist degrades (a
//!    logout-all'd token lives until exp, ≤ 15 min — the same
//!    residual window as the single logout).
//!
//! Both rejections are `PF-1010` (invalid token) — the same
//! code as a bad signature. A revoked token is not
//! distinguishable from a forged one: the response never says
//! WHY the token is invalid.
//!
//! The admin guard is [`AdminContext`] (P1-008): it wraps this
//! extractor and additionally requires the role to be
//! [`Role::Admin`], rejecting a verified non-admin (user or
//! support) with 403 `PF-1002`. Admin handlers take it instead
//! of [`AuthContext`] so the role check cannot be forgotten.
//! Support is read-only: its readable routes take
//! [`AdminReadContext`] (P1-009), which admits admin + support
//! and rejects a verified plain user with the same 403.
//!
//! # Credential sources (precedence — FINAL, P1-016)
//!
//! 1. `Authorization: Bearer <token>` — a JWT (two dots:
//!    `header.payload.signature`) or an API key (anything else
//!    in the Bearer slot). The two-dot shape is the Phase 0
//!    heuristic; Phase 1 may switch to an API-key prefix.
//! 2. The `detag_refresh` cookie (web session, httpOnly refresh
//!    token — FR-A-02). The cookie name is `detag_refresh`,
//!    confirmed final by P1-016 (login sets it, the refresh
//!    handler reads it, this extractor classifies it).
//!
//! A Bearer token wins over a cookie when both are present: an
//! API client that sends a Bearer token means it — and the
//! cookie is then IGNORED, never validated as an access
//! credential (a forged or stale cookie alongside a valid
//! Bearer token does not fail the request; a valid refresh
//! token alongside a Bearer token does not add anything).
//! A non-Bearer `Authorization` scheme (e.g. `Basic`) is
//! ignored — the spec defines Bearer only.
//!
//! # CSRF posture (P1-016)
//!
//! The only cookie the api ever sets is the refresh cookie —
//! `HttpOnly; Secure; SameSite=Lax; Path=/v1/auth;
//! Max-Age=2592000` — and the posture is the combination, not
//! any one attribute:
//! - `SameSite=Lax`: a cross-site `fetch`/XHR POST (the classic
//!   CSRF shape — a form on evil.com posting to the api) does
//!   NOT carry the cookie, so `POST /v1/auth/refresh` from
//!   another origin has no token to rotate. A top-level GET
//!   navigation DOES carry it — but there is no state-changing
//!   GET: every mutation is POST/PATCH/DELETE, every GET is a
//!   read, and `GET /v1/auth/refresh` is 405 (proven by
//!   `tests/dual_auth.rs`), so a navigated GET cannot rotate a
//!   session or change anything.
//! - `Path=/v1/auth`: the browser sends the cookie only to
//!   `/v1/auth/*` — never to a protected route (`/v1/me`,
//!   `/v1/admin/*`), so the cookie cannot be an access
//!   credential for the API even by accident. This is why the
//!   extractor's bare-cookie rejection is not a limitation to
//!   be fixed later: from a real browser the extractor never
//!   sees the cookie on a protected route at all.
//! - `HttpOnly`: JavaScript cannot read the refresh token, so
//!   an XSS payload cannot exfiltrate it. Honest limit: that
//!   is defense-in-depth, not a cure — XSS running on the
//!   same origin can still call `/v1/auth/refresh` itself (the
//!   browser attaches the cookie same-site) and use the
//!   access token it gets. The refresh token staying out of
//!   JS keeps it out of `localStorage` dumps and stolen-page
//!   scripts; it does not make XSS harmless.
//! - `Secure`: the cookie only travels over HTTPS in
//!   production. In dev the api runs on plain HTTP —
//!   `http://localhost` is a secure context, so browsers
//!   still send a `Secure` cookie there; a non-localhost
//!   plain-HTTP dev host would not get the cookie (by design).
//! - The access token is never a cookie: it lives in the web
//!   client's memory and travels as `Bearer`. A stolen access
//!   token is worth at most 15 minutes (P1-013 claims shape),
//!   and the refresh token that mints the next one never
//!   appears in a request body or a log line.
//!
//! # Never logged
//!
//! The credential value never appears in a `Debug` rendering, a
//! log line, or an error body: [`Credential`]'s `Debug` prints
//! the kind with `[redacted]`, and the error body for auth
//! failures is the variant's static safe detail (P0-032).

use axum::{
    extract::{FromRequestParts, MatchedPath},
    http::{header, request::Parts, HeaderMap, Method},
    response::{IntoResponse, Response},
};

use crate::rate_limit::{check_api_key_rate_limit, denied_response};
use crate::services::api_keys::{note_key_use, verify_api_key, VerifyApiKeyError};
use crate::services::revocation::is_jti_revoked;
use crate::{problem::ProblemResponse, request_id::RequestId, AppState};
use detag_core::{
    error::{AppError, Profile},
    jwt::{verify_access_token_at, JwtError},
    repos::users::{self, User},
};

/// The credential the caller presented, if any. The VALUE is
/// secret: `Debug` prints only the kind (see the module docs).
#[derive(Clone, PartialEq, Eq)]
pub enum Credential {
    /// `Authorization: Bearer <jwt>` — three base64url segments.
    Bearer(String),
    /// `Authorization: Bearer <key>` — an API key (not a JWT).
    ApiKey(String),
    /// The `detag_refresh` cookie — the web session's refresh token.
    Session(String),
    /// No credential was presented.
    None,
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Credential::Bearer(_) => write!(f, "Bearer([redacted])"),
            Credential::ApiKey(_) => write!(f, "ApiKey([redacted])"),
            Credential::Session(_) => write!(f, "Session([redacted])"),
            Credential::None => write!(f, "None"),
        }
    }
}

impl Credential {
    /// The kind of credential, for logs and tests — never the value.
    pub fn kind(&self) -> &'static str {
        match self {
            Credential::Bearer(_) => "bearer",
            Credential::ApiKey(_) => "api-key",
            Credential::Session(_) => "session",
            Credential::None => "none",
        }
    }
}

/// Caller roles (api-spec: admin role; support gets a read-only
/// subset).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// No verified identity.
    Anonymous,
    /// A verified user.
    User,
    /// A verified admin.
    Admin,
    /// A verified support agent (read-only subset).
    Support,
}

impl Role {
    /// The role for a user row's `role` string (`user` /
    /// `support` / `admin` — the column CHECK admits nothing
    /// else, so anything unrecognized is a plain user).
    fn from_user_role(role: &str) -> Self {
        match role {
            "admin" => Role::Admin,
            "support" => Role::Support,
            _ => Role::User,
        }
    }
}

/// A verified API key (P1-034, FR-A-06): what the extractor
/// attaches to the context after the hash lookup + scope
/// check pass. Carries NO secret material — only the key id
/// (the revoke/rotate target, and the `key_id` P1-038's job
/// attribution will record), the scopes the key may use,
/// and the row's rate-limit tier (`None` = the default tier;
/// P1-037 enforces tiers, this ticket only resolves them).
/// `Some` exactly when the caller authenticated with a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedApiKey {
    /// The `api_keys` row id.
    pub id: uuid::Uuid,
    /// The scopes stored on the row (`jobs:write`, `jobs:read`,
    /// `webhooks:manage` — each checked independently).
    pub scopes: Vec<String>,
    /// The row's `rate_limit_tier` (`None` = default tier).
    pub rate_limit_tier: Option<String>,
}

/// Who the caller is — as far as Phase 0 knows.
///
/// The struct is deliberately plain data: handlers read
/// [`AuthContext::credential`], [`AuthContext::role`], and
/// [`AuthContext::verified`]. Nothing here is proof of identity
/// until Phase 1 sets `verified` from a real check.
#[derive(Debug, Clone, PartialEq)]
pub struct AuthContext {
    /// The presented credential (or [`Credential::None`]).
    pub credential: Credential,
    /// The caller's role. Always [`Role::Anonymous`] in Phase 0.
    pub role: Role,
    /// Whether the credential was verified. Always `false` in
    /// Phase 0 — see the module docs.
    pub verified: bool,
    /// The verified user, when a Bearer <redacted> JWT verified and
    /// its `sub` loaded (P1-005). `None` for every unverified
    /// context — handlers that need the user take the context
    /// and read this (it is `Some` exactly when `verified`).
    pub user: Option<User>,
    /// The verified API key, when the caller authenticated
    /// with one (P1-034). `None` for JWT callers and for every
    /// unverified context. Handlers that attribute work to a
    /// key (P1-038) read `api_key.id` here.
    pub api_key: Option<VerifiedApiKey>,
    /// The impersonating admin's id, when the verified token is
    /// an impersonation token (P1-010): `Some` exactly when the
    /// token's claims carry `impersonator_id`. `None` on a normal
    /// login token and on every unverified context. Handlers
    /// that act for the user read `user`; the admin guards
    /// reject any context where this is `Some` (an impersonator
    /// cannot reach admin endpoints — P1-011 tests that).
    pub impersonator_id: Option<uuid::Uuid>,
}

/// The web session cookie name (confirmed final by P1-016:
/// `detag_refresh`, `HttpOnly; Secure; SameSite=Lax;
/// Path=/v1/auth` — login sets it, the refresh handler reads
/// it, this extractor classifies it and rejects it as an
/// access credential; see the module docs).
pub const REFRESH_COOKIE: &str = "detag_refresh";

impl AuthContext {
    /// The user id to record as the audit actor for an action
    /// this context performs (P1-011): the impersonator when
    /// the caller is impersonating (the human who acted), else
    /// the user themself. The audit row's `target_id` still
    /// names the user acted-as — so an impersonated action's
    /// row carries both ids in the standard columns, and a
    /// normal action's row is unchanged (actor == target).
    pub fn audit_actor_id(&self, user: &User) -> uuid::Uuid {
        self.impersonator_id.unwrap_or(user.id)
    }

    /// Parse the credential from request headers. Pure (no I/O,
    /// no verification) — unit-tested here; the extractor below
    /// calls it, then verifies.
    pub fn from_headers(headers: &HeaderMap) -> Self {
        let credential = parse_bearer(headers)
            .or_else(|| parse_session_cookie(headers))
            .unwrap_or(Credential::None);
        // Parsed, never verified (the extractor verifies).
        Self {
            credential,
            role: Role::Anonymous,
            verified: false,
            user: None,
            api_key: None,
            impersonator_id: None,
        }
    }

    /// Require one scope of the caller (P1-034, FR-A-06): the
    /// handler-level scope check for routes the extractor's
    /// table does not cover (or that need a scope beyond the
    /// route's default). A verified JWT caller is not
    /// scope-limited — a user token carries the user's full
    /// rights — so this passes for them; a verified key caller
    /// must carry the scope, else 403 `PF-1002`; an unverified
    /// context (which a handler never sees — the extractor
    /// rejects it first) fails closed.
    pub fn require_scope(&self, scope: &str) -> Result<(), AppError> {
        match &self.api_key {
            Some(key) => {
                if key.scopes.iter().any(|s| s == scope) {
                    Ok(())
                } else {
                    Err(AppError::Forbidden)
                }
            }
            None => {
                if self.verified {
                    Ok(())
                } else {
                    Err(AppError::Forbidden)
                }
            }
        }
    }
}

/// `Authorization: Bearer <token>` → [`Credential::Bearer`] for a
/// JWT-shaped token (two dots), [`Credential::ApiKey`] otherwise.
/// A missing header, a non-Bearer scheme, or an empty token yields
/// `None` (the caller may still have a session cookie).
fn parse_bearer(headers: &HeaderMap) -> Option<Credential> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let token = value
        .split_once(' ')
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .map(|(_, token)| token.trim())
        .filter(|token| !token.is_empty())?;
    if token.matches('.').count() == 2 {
        Some(Credential::Bearer(token.to_string()))
    } else {
        Some(Credential::ApiKey(token.to_string()))
    }
}

/// The `detag_refresh` cookie → [`Credential::Session`]. Parses
/// `Cookie` headers by hand (`;`-separated `name=value` pairs,
/// split on the FIRST `=` so base64 padding survives) instead of
/// adding a cookie crate for one lookup. `pub(crate)`: the
/// refresh handler (P1-014) reads the cookie the same way the
/// extractor does.
pub(crate) fn parse_session_cookie(headers: &HeaderMap) -> Option<Credential> {
    for cookie_header in headers.get_all(header::COOKIE).iter() {
        let text = cookie_header.to_str().ok()?;
        for pair in text.split(';') {
            let pair = pair.trim();
            let Some((name, value)) = pair.split_once('=') else {
                continue;
            };
            if name.trim() == REFRESH_COOKIE && !value.trim().is_empty() {
                return Some(Credential::Session(value.trim().to_string()));
            }
        }
    }
    None
}

/// The scope an API key needs for a route (P1-034, FR-A-06),
/// by method + route template (the `MatchedPath` the router
/// matched — `{id}` stays a template, never a concrete id).
/// `None` means keys may NEVER call the route: the extractor
/// rejects a key caller there with 403 `PF-1002` (fail
/// closed). JWT callers are unaffected — scopes limit keys,
/// not users.
///
/// The table is deliberately small: only the routes whose
/// handler takes `AuthContext` today (jobs + webhook
/// endpoints). P1-040's ACL matrix owns the full table — when
/// a new handler lands with a scope, its row goes here AND in
/// the matrix. Routes that do not take `AuthContext` yet
/// (uploads, plans, billing stubs) run no extractor at all,
/// so a key reaching them is that handler ticket's gap to
/// close, not this table's.
fn required_scope_for(method: &Method, path: &str) -> Option<&'static str> {
    match (method.as_str(), path) {
        // Jobs: writes create/cancel/retry/attest, reads list/get.
        ("POST", "/v1/jobs") => Some("jobs:write"),
        ("GET", "/v1/jobs") => Some("jobs:read"),
        ("GET", "/v1/jobs/{id}") => Some("jobs:read"),
        ("POST", "/v1/jobs/{id}/cancel")
        | ("POST", "/v1/jobs/{id}/retry")
        | ("POST", "/v1/jobs/{id}/attestation") => Some("jobs:write"),
        ("GET", "/v1/jobs/{id}/events")
        | ("GET", "/v1/jobs/{id}/result")
        | ("GET", "/v1/jobs/{id}/report") => Some("jobs:read"),
        // The user's own webhook endpoints are managed by key.
        ("GET", "/v1/webhook-endpoints")
        | ("POST", "/v1/webhook-endpoints")
        | ("DELETE", "/v1/webhook-endpoints/{id}")
        | ("GET", "/v1/webhook-endpoints/{id}/deliveries") => Some("webhooks:manage"),
        // Everything else — /v1/me, /v1/api-keys, /v1/admin/*,
        // uploads, billing, auth — rejects every key.
        _ => None,
    }
}

/// The Bearer verification (P1-005): the extractor takes
/// [`AppState`] (the pool + the JWT secret) and rejects with a
/// problem+json 401 — never an `Ok` unverified context. A
/// handler that declares `AuthContext` therefore only ever
/// runs for a verified caller; the `user` is `Some` exactly
/// when `verified` is true. API keys verify by hash lookup +
/// scope check (P1-034 — see the module docs); a bare session
/// cookie rejects with `PF-1001` as a FINAL decision (P1-016 — the
/// refresh token is not an access credential, and its
/// `Path=/v1/auth` means a browser never sends it to a
/// protected route anyway; see the module docs).
impl FromRequestParts<AppState> for AuthContext {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let request_id = parts.extensions.get::<RequestId>().map(|id| id.0.as_str());
        // A rejection helper: the problem+json body carries the
        // request id, like every other error (the middleware's
        // request-id layer runs outside this extractor, so the
        // id is read from the extensions here).
        let reject = |err: AppError| {
            ProblemResponse(err.to_problem(Profile::Production, request_id, None)).into_response()
        };
        let ctx = Self::from_headers(&parts.headers);
        // Destructure once: the token moves out of the credential
        // here and is rebuilt into the verified context below.
        let AuthContext { credential, .. } = ctx;
        let token = match credential {
            Credential::None => return Err(reject(AppError::Unauthenticated)),
            // The API-key leg (P1-034): hash lookup + owner
            // load (`verify_api_key`), then the scope check for
            // this route (see the module docs). An unknown hash,
            // a revoked key, a missing owner, or a non-`active`
            // owner all answer the generic 401 `PF-1001` — the
            // response never says which (P1-016's indistinguishability
            // rule, applied to keys). A key without the route's
            // scope — or on a route the table does not name — is
            // 403 `PF-1002`. A DB failure is 500, like the JWT
            // user lookup below. A verified key use re-stamps
            // `last_used_at` (throttled, 300 s) — only after the
            // scope check passes, so a denied request is not a use.
            Credential::ApiKey(presented) => {
                let (key, user) = match verify_api_key(state, &presented).await {
                    Ok(pair) => pair,
                    Err(VerifyApiKeyError::Unknown) => {
                        return Err(reject(AppError::Unauthenticated));
                    }
                    Err(VerifyApiKeyError::Internal) => {
                        return Err(reject(AppError::Internal));
                    }
                };
                let path = parts
                    .extensions
                    .get::<MatchedPath>()
                    .map(|m| m.as_str())
                    .unwrap_or("");
                let required = required_scope_for(&parts.method, path);
                let Some(required) = required else {
                    return Err(reject(AppError::Forbidden));
                };
                if !key.scopes.iter().any(|scope| scope == required) {
                    return Err(reject(AppError::Forbidden));
                }
                // The per-key rate limit (P1-037, FR-A-06): the
                // key's tier bucket spends one token per verified
                // request. A denial is 429 `PF-3007` with the
                // key bucket's `X-RateLimit-*` trio + `Retry-After`
                // — BEFORE `note_key_use`, so a denied request
                // is not a use (the P1-034 rule, applied to the
                // rate limit too). A Redis error fails OPEN (warn
                // logged): the P0-103 decision — a cache outage
                // must not take auth down; the scope check
                // already ran, so only the rate limit degrades.
                match check_api_key_rate_limit(state, &key).await {
                    Ok(decision) => {
                        if !decision.result.allowed {
                            return Err(denied_response(
                                decision.result,
                                decision.limit,
                                request_id,
                            ));
                        }
                    }
                    Err(err) => {
                        tracing::warn!(
                            error = %err,
                            key_id = %key.id,
                            "api-keys: rate-limit check failed, failing open"
                        );
                    }
                }
                note_key_use(state, &key).await;
                return Ok(Self {
                    credential: Credential::ApiKey(presented),
                    role: Role::from_user_role(&user.role),
                    verified: true,
                    user: Some(user),
                    api_key: Some(VerifiedApiKey {
                        id: key.id,
                        scopes: key.scopes,
                        rate_limit_tier: key.rate_limit_tier,
                    }),
                    impersonator_id: None,
                });
            }
            Credential::Session(_) => {
                return Err(reject(AppError::Unauthenticated));
            }
            Credential::Bearer(token) => token,
        };
        // The JWT verifies first (no DB touch for a bad token):
        // an invalid signature or malformed token is PF-1010, an
        // expired one PF-1011. Verified against the injected
        // clock (P1-023), not the wall clock.
        let claims =
            match verify_access_token_at(&state.settings.jwt_secret, &token, state.clock.now()) {
                Ok(claims) => claims,
                Err(JwtError::Expired) => return Err(reject(AppError::ExpiredToken)),
                Err(_) => return Err(reject(AppError::InvalidToken)),
            };
        // The `sub` is the user id we issued (P1-003). A token
        // whose `sub` is not a UUID is not a token we issued.
        let user_id = match claims.sub.parse::<uuid::Uuid>() {
            Ok(id) => id,
            Err(_) => return Err(reject(AppError::InvalidToken)),
        };
        let user = match users::by_id(&state.pool, user_id).await {
            Ok(user) => user,
            Err(err) => {
                tracing::error!(error = ?err, "auth: user lookup failed");
                return Err(reject(AppError::Internal));
            }
        };
        let Some(user) = user else {
            // The token is well-formed but the account is gone.
            return Err(reject(AppError::Unauthenticated));
        };
        // Revocation, epoch first (P1-021): a token issued
        // before the user's `revoked_before` is dead — a
        // password change bumped the epoch, and this token
        // predates it. Zero extra queries (the column rides
        // the loaded row). The comparison is `<=`, not `<`:
        // `iat` is whole seconds while the epoch is
        // microsecond-precise, so a token minted in the same
        // second as the bump (before OR after it) shares the
        // bump's second — and a pre-change token in that
        // second must die (strict `<` has a one-second hole
        // here: `iat == floor(bump)` is not `< bump`, so the
        // token survives — caught by the revocation tests).
        // A post-change token minted in the same second is
        // denied too and self-heals via refresh (its refresh
        // token is still valid; the next mint lands in a
        // later second). Fail closed on the boundary second.
        if let Some(revoked_before) = user.revoked_before {
            if claims.iat <= revoked_before.timestamp() {
                return Err(reject(AppError::InvalidToken));
            }
        }
        // Revocation, jti denylist second (P1-021): one Redis
        // GET on the token's jti. A token without a jti
        // (pre-P1-013 legacy) skips the check — the epoch
        // above still covers it via `iat`. A Redis error
        // fails OPEN (warn logged): the epoch is the durable
        // backstop, the denylist is the fast path.
        if let Some(jti) = claims.jti {
            match is_jti_revoked(state, &jti).await {
                Ok(true) => return Err(reject(AppError::InvalidToken)),
                Ok(false) => {}
                Err(err) => {
                    tracing::warn!(error = ?err, "auth: revocation cache unavailable, failing open");
                }
            }
        }
        Ok(Self {
            credential: Credential::Bearer(token),
            role: Role::from_user_role(&user.role),
            verified: true,
            user: Some(user),
            api_key: None,
            // The impersonation marker rides the claims (P1-010):
            // a token minted by `issue_impersonation_token`
            // carries the admin's id here, and the admin guards
            // reject the context on that basis. The `role` above
            // is the TARGET user's role — the token acts as them.
            impersonator_id: claims.impersonator_id,
        })
    }
}

/// An admin-only context (P1-008): the verified [`AuthContext`]
/// plus the guarantee that the caller's role is [`Role::Admin`].
///
/// Handlers that need admin rights take this instead of
/// [`AuthContext`] — the role check runs in the extractor, so a
/// handler body can never forget it. The rejections, in order:
/// no credential / bad token / unknown user → the same 401s as
/// `AuthContext`; a verified non-admin (user or support) → 403
/// `PF-1002`; a context carrying `impersonator_id` → 403
/// `PF-1002` (P1-010 — an impersonator cannot reach admin
/// endpoints, not even to impersonate someone else). Support is
/// read-only: its readable routes (admin search, P1-009) take
/// [`AdminReadContext`], not this one.
#[derive(Debug, Clone)]
pub struct AdminContext {
    /// The verified auth context — the admin user is always
    /// `Some` (a verified context always carries its user).
    pub auth: AuthContext,
}

impl FromRequestParts<AppState> for AdminContext {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let auth = AuthContext::from_request_parts(parts, state).await?;
        if auth.role != Role::Admin {
            let request_id = parts.extensions.get::<RequestId>().map(|id| id.0.as_str());
            return Err(ProblemResponse(AppError::Forbidden.to_problem(
                Profile::Production,
                request_id,
                None,
            ))
            .into_response());
        }
        // An API key cannot reach admin endpoints (P1-034,
        // FR-A-06 — "no admin ever", P1-040): the extractor's
        // scope table already rejects keys on every admin
        // route, but the guard is the second line — a key
        // minted by an admin user must not admin through it
        // even if a future ticket mis-rows the table. Same
        // 403 PF-1002 as a wrong role.
        if auth.api_key.is_some() {
            let request_id = parts.extensions.get::<RequestId>().map(|id| id.0.as_str());
            return Err(ProblemResponse(AppError::Forbidden.to_problem(
                Profile::Production,
                request_id,
                None,
            ))
            .into_response());
        }
        // An impersonator cannot reach admin endpoints (P1-010):
        // the token acts as the target user, and the admin who
        // impersonated them must not use that token to act as an
        // admin — not even to impersonate someone else. The
        // rejection is the same 403 PF-1002 as a wrong role.
        if auth.impersonator_id.is_some() {
            let request_id = parts.extensions.get::<RequestId>().map(|id| id.0.as_str());
            return Err(ProblemResponse(AppError::Forbidden.to_problem(
                Profile::Production,
                request_id,
                None,
            ))
            .into_response());
        }
        Ok(Self { auth })
    }
}

/// A read-only admin context (P1-009): the verified
/// [`AuthContext`] plus the guarantee that the caller's role
/// is [`Role::Admin`] or [`Role::Support`].
///
/// Support is read-only: it may use the readable admin routes
/// (user search, dashboard, audit query) but never a write —
/// the write routes take [`AdminContext`], which rejects
/// support with 403 `PF-1002`. The rejections, in order: no
/// credential / bad token / unknown user → the same 401s as
/// `AuthContext`; a verified plain user → 403 `PF-1002`; a
/// context carrying `impersonator_id` → 403 `PF-1002` (P1-010
/// — an impersonator cannot reach admin endpoints, reads
/// included).
#[derive(Debug, Clone)]
pub struct AdminReadContext {
    /// The verified auth context — the caller is always `Some`
    /// (a verified context always carries its user).
    pub auth: AuthContext,
}

impl FromRequestParts<AppState> for AdminReadContext {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let auth = AuthContext::from_request_parts(parts, state).await?;
        if auth.role != Role::Admin && auth.role != Role::Support {
            let request_id = parts.extensions.get::<RequestId>().map(|id| id.0.as_str());
            return Err(ProblemResponse(AppError::Forbidden.to_problem(
                Profile::Production,
                request_id,
                None,
            ))
            .into_response());
        }
        // An API key cannot reach admin endpoints (P1-034) —
        // reads included, same second line as `AdminContext`.
        if auth.api_key.is_some() {
            let request_id = parts.extensions.get::<RequestId>().map(|id| id.0.as_str());
            return Err(ProblemResponse(AppError::Forbidden.to_problem(
                Profile::Production,
                request_id,
                None,
            ))
            .into_response());
        }
        // An impersonator cannot reach admin endpoints either
        // (P1-010) — reads included: the token acts as the
        // target user, and the admin who impersonated them must
        // not use it to read the admin list, the dashboard, or
        // the audit trail. Same 403 PF-1002 as a wrong role.
        if auth.impersonator_id.is_some() {
            let request_id = parts.extensions.get::<RequestId>().map(|id| id.0.as_str());
            return Err(ProblemResponse(AppError::Forbidden.to_problem(
                Profile::Production,
                request_id,
                None,
            ))
            .into_response());
        }
        Ok(Self { auth })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request, response::IntoResponse, routing::get, Json, Router};
    use tower::ServiceExt;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(
                header::HeaderName::from_bytes(name.as_bytes()).expect("header name parses"),
                header::HeaderValue::from_str(value).expect("header value parses"),
            );
        }
        map
    }

    /// A JWT-shaped token (two dots, three segments).
    const JWT: &str = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.signature";
    /// An API key (no dots).
    const API_KEY: &str = "pf_live_01J0000000000000000000000";

    #[test]
    fn bearer_jwt_is_classified_jwt() {
        let ctx =
            AuthContext::from_headers(&headers(&[("authorization", &format!("Bearer {JWT}"))]));
        assert_eq!(ctx.credential, Credential::Bearer(JWT.to_string()));
        assert_eq!(ctx.credential.kind(), "bearer");
        assert_eq!(ctx.role, Role::Anonymous);
        assert!(!ctx.verified, "from_headers parses; the extractor verifies");
    }

    #[test]
    fn bearer_api_key_is_classified_api_key() {
        let ctx =
            AuthContext::from_headers(&headers(&[("authorization", &format!("Bearer {API_KEY}"))]));
        assert_eq!(ctx.credential, Credential::ApiKey(API_KEY.to_string()));
        assert_eq!(ctx.credential.kind(), "api-key");
        assert!(!ctx.verified);
    }

    #[test]
    fn bearer_scheme_is_case_insensitive_and_token_is_trimmed() {
        let ctx = AuthContext::from_headers(&headers(&[(
            "authorization",
            &format!("bEaReR   {API_KEY}  "),
        )]));
        assert_eq!(ctx.credential, Credential::ApiKey(API_KEY.to_string()));
    }

    #[test]
    fn non_bearer_scheme_is_ignored() {
        let ctx = AuthContext::from_headers(&headers(&[("authorization", "Basic dXNlcjpwYXNz")]));
        assert_eq!(ctx.credential, Credential::None);
    }

    #[test]
    fn empty_bearer_is_no_credential() {
        let ctx = AuthContext::from_headers(&headers(&[("authorization", "Bearer   ")]));
        assert_eq!(ctx.credential, Credential::None);
    }

    #[test]
    fn session_cookie_is_parsed() {
        let ctx = AuthContext::from_headers(&headers(&[(
            "cookie",
            "other=1; detag_refresh=refresh-token-abc; theme=dark",
        )]));
        assert_eq!(
            ctx.credential,
            Credential::Session("refresh-token-abc".to_string())
        );
        assert_eq!(ctx.credential.kind(), "session");
    }

    #[test]
    fn bearer_jwt_wins_over_cookie() {
        // Precedence, P1-016: a Bearer JWT beats the session
        // cookie even when the cookie is present — the cookie
        // is then ignored, never validated as an access
        // credential (the integration tests in tests/dual_auth.rs
        // prove the same thing end to end, with a real refresh
        // token in the cookie).
        let ctx = AuthContext::from_headers(&headers(&[
            ("authorization", &format!("Bearer {JWT}")),
            ("cookie", "detag_refresh=refresh-token-abc"),
        ]));
        assert_eq!(ctx.credential, Credential::Bearer(JWT.to_string()));
        assert_eq!(ctx.credential.kind(), "bearer");
    }

    #[test]
    fn bearer_wins_over_cookie() {
        let ctx = AuthContext::from_headers(&headers(&[
            ("authorization", &format!("Bearer {API_KEY}")),
            ("cookie", "detag_refresh=refresh-token-abc"),
        ]));
        assert_eq!(ctx.credential, Credential::ApiKey(API_KEY.to_string()));
    }

    #[test]
    fn no_credential_is_none() {
        let ctx = AuthContext::from_headers(&headers(&[]));
        assert_eq!(ctx.credential, Credential::None);
        assert_eq!(ctx.credential.kind(), "none");
        assert_eq!(ctx.role, Role::Anonymous);
        assert!(!ctx.verified);
    }

    #[test]
    fn debug_never_shows_a_credential_value() {
        for cred in [
            Credential::Bearer(JWT.to_string()),
            Credential::ApiKey(API_KEY.to_string()),
            Credential::Session("refresh-token-abc".to_string()),
        ] {
            let rendered = format!("{cred:?}");
            assert!(
                !rendered.contains(JWT)
                    && !rendered.contains(API_KEY)
                    && !rendered.contains("refresh-token-abc"),
                "credential value leaked into Debug: {rendered}"
            );
            assert!(rendered.contains("[redacted]"), "{rendered}");
        }
        let ctx =
            AuthContext::from_headers(&headers(&[("authorization", &format!("Bearer {JWT}"))]));
        let rendered = format!("{ctx:?}");
        assert!(
            !rendered.contains(JWT),
            "AuthContext Debug leaked the token: {rendered}"
        );
    }

    /// The extractor end to end: a probe handler that echoes
    /// the parsed context runs only for a verified caller — the
    /// three no-DB rejections (no credential, malformed JWT,
    /// expired JWT) never reach it and answer problem+json 401s
    /// with the taxonomy codes. The happy path (a valid JWT for
    /// a real user) needs a database, so it lives in the
    /// integration tests (`tests/profile.rs`), not here. The
    /// cookie case below uses a fake token; the REAL-token case
    /// (a live refresh token presented as the sole credential
    /// still 401s) is `tests/dual_auth.rs` (P1-016). An API key
    /// is NOT in this table any more: since P1-034 the key arm
    /// reads the database (hash lookup), so its cases live in
    /// the integration tests (`tests/extractor.rs`) — a unit
    /// test here would depend on the dev database being up.
    #[tokio::test]
    async fn extractor_rejects_unverified_callers() {
        async fn probe(ctx: AuthContext) -> impl IntoResponse {
            Json(serde_json::json!({
                "kind": ctx.credential.kind(),
                "role": format!("{:?}", ctx.role),
                "verified": ctx.verified,
            }))
        }
        let settings = detag_core::config::Settings::default();
        let state = AppState::from_settings(&settings).expect("test state builds");
        let app = Router::new().route("/probe", get(probe)).with_state(state);
        let expired = detag_core::jwt::issue_access_token(
            &settings.jwt_secret,
            uuid::Uuid::now_v7(),
            "user",
            -60,
        )
        .expect("expired token issues");

        for (headers, want_code) in [
            (None, "PF-1001"),
            (Some(format!("Bearer {JWT}")), "PF-1010"),
            (Some(format!("Bearer {expired}")), "PF-1011"),
            (
                Some("detag_refresh=refresh-token-abc".to_string()),
                "PF-1001",
            ),
        ] {
            let mut req = Request::builder().uri("/probe");
            if let Some(h) = &headers {
                if h.starts_with("detag_refresh=") {
                    req = req.header("cookie", h);
                } else {
                    req = req.header("authorization", h);
                }
            }
            let res = app
                .clone()
                .oneshot(req.body(Body::empty()).unwrap())
                .await
                .expect("oneshot probe");
            assert_eq!(res.status(), 401, "headers {headers:?}");
            let body = axum::body::to_bytes(res.into_body(), usize::MAX)
                .await
                .expect("read body");
            let json: serde_json::Value =
                serde_json::from_slice(&body).expect("probe body is JSON");
            assert_eq!(json["code"], want_code, "headers {headers:?}");
        }
    }

    #[test]
    fn scope_table_names_the_route_scopes_and_nothing_else() {
        // P1-034: the table is the extractor's enforcement —
        // jobs writes need jobs:write, jobs reads jobs:read,
        // webhook endpoints webhooks:manage; every other route
        // (and every wrong method) is None, i.e. keys denied.
        use axum::http::Method;
        for (method, path, want) in [
            (Method::POST, "/v1/jobs", Some("jobs:write")),
            (Method::GET, "/v1/jobs", Some("jobs:read")),
            (Method::GET, "/v1/jobs/{id}", Some("jobs:read")),
            (Method::POST, "/v1/jobs/{id}/cancel", Some("jobs:write")),
            (Method::POST, "/v1/jobs/{id}/retry", Some("jobs:write")),
            (
                Method::POST,
                "/v1/jobs/{id}/attestation",
                Some("jobs:write"),
            ),
            (Method::GET, "/v1/jobs/{id}/events", Some("jobs:read")),
            (Method::GET, "/v1/jobs/{id}/result", Some("jobs:read")),
            (Method::GET, "/v1/jobs/{id}/report", Some("jobs:read")),
            (
                Method::GET,
                "/v1/webhook-endpoints",
                Some("webhooks:manage"),
            ),
            (
                Method::POST,
                "/v1/webhook-endpoints",
                Some("webhooks:manage"),
            ),
            (
                Method::DELETE,
                "/v1/webhook-endpoints/{id}",
                Some("webhooks:manage"),
            ),
            (
                Method::GET,
                "/v1/webhook-endpoints/{id}/deliveries",
                Some("webhooks:manage"),
            ),
            // Unmapped routes and wrong methods deny keys.
            (Method::GET, "/v1/me", None),
            (Method::POST, "/v1/api-keys", None),
            (Method::GET, "/v1/admin/users", None),
            (Method::POST, "/v1/uploads", None),
            (Method::DELETE, "/v1/jobs/{id}", None),
            (Method::GET, "/v1/jobs/{id}/cancel", None),
            (Method::POST, "/v1/jobs/{id}", None),
        ] {
            assert_eq!(required_scope_for(&method, path), want, "{method} {path}");
        }
    }

    #[test]
    fn require_scope_passes_jwt_and_scoped_keys_only() {
        // P1-034: a verified JWT caller is not scope-limited;
        // a key caller must carry the scope; an unverified
        // context fails closed.
        let jwt_ctx = AuthContext {
            credential: Credential::Bearer(JWT.to_string()),
            role: Role::User,
            verified: true,
            user: None,
            api_key: None,
            impersonator_id: None,
        };
        assert!(jwt_ctx.require_scope("jobs:write").is_ok());
        assert!(jwt_ctx.require_scope("admin").is_ok());

        let key_ctx = AuthContext {
            credential: Credential::ApiKey(API_KEY.to_string()),
            role: Role::User,
            verified: true,
            user: None,
            api_key: Some(VerifiedApiKey {
                id: uuid::Uuid::now_v7(),
                scopes: vec!["jobs:read".to_string()],
                rate_limit_tier: None,
            }),
            impersonator_id: None,
        };
        assert!(key_ctx.require_scope("jobs:read").is_ok());
        assert!(matches!(
            key_ctx.require_scope("jobs:write"),
            Err(AppError::Forbidden)
        ));

        let unverified =
            AuthContext::from_headers(&headers(&[("authorization", &format!("Bearer {API_KEY}"))]));
        assert!(matches!(
            unverified.require_scope("jobs:read"),
            Err(AppError::Forbidden)
        ));
    }
}
