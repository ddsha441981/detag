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
//! # Phase 1 status — the Bearer leg is verified (P1-005)
//!
//! A `Bearer <jwt>` is verified (HS256 signature + expiry, leeway 0)
//! against `PF_JWT_SECRET` and the `sub` claim is loaded as a user:
//! a verified context carries the [`User`] (`verified: true`, the real
//! [`Role`] from the user row). The rejections:
//! - no credential → 401 `PF-1001`;
//! - a Bearer token that is not a valid JWT → 401 `PF-1010`;
//!   an expired one → 401 `PF-1011`;
//! - a valid JWT for an unknown user id → 401 `PF-1001` (the token
//!   is well-formed but the account is gone);
//! - a Bearer API key or a session cookie → 401 `PF-1001` for now:
//!   API keys are a later ticket (no key table is read yet) and
//!   the refresh cookie is not an access credential (the refresh
//!   rotation is P1-014; the cookie leg is P1-016).
//!
//! P1-016 owns the rest: the cookie + API-key legs, the precedence
//! (Bearer wins today, and stays winning), and the CSRF posture
//! documentation. A suspended/deleted user is NOT rejected here:
//! suspension revokes sessions at suspend time (P1-008), and the
//! stateless access token's remaining lifetime (≤ 15 min) is that
//! ticket's design space, not this extractor's.
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
//! # Credential sources (precedence)
//!
//! 1. `Authorization: Bearer <token>` — a JWT (two dots:
//!    `header.payload.signature`) or an API key (anything else
//!    in the Bearer slot). The two-dot shape is the Phase 0
//!    heuristic; Phase 1 may switch to an API-key prefix.
//! 2. The `detag_refresh` cookie (web session, httpOnly refresh
//!    token — FR-A-02). The cookie NAME is provisional: Phase 1
//!    owns both sides (login sets it, this extractor reads it)
//!    and confirms or renames it then.
//!
//! A Bearer token wins over a cookie when both are present: an
//! API client that sends a Bearer token means it. A non-Bearer
//! `Authorization` scheme (e.g. `Basic`) is ignored — the spec
//! defines Bearer only.
//!
//! # Never logged
//!
//! The credential value never appears in a `Debug` rendering, a
//! log line, or an error body: [`Credential`]'s `Debug` prints
//! the kind with `[redacted]`, and the error body for auth
//! failures is the variant's static safe detail (P0-032).

use axum::{
    extract::FromRequestParts,
    http::{header, request::Parts, HeaderMap},
    response::{IntoResponse, Response},
};

use crate::{problem::ProblemResponse, request_id::RequestId, AppState};
use detag_core::{
    error::{AppError, Profile},
    jwt::{verify_access_token, JwtError},
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
    /// The impersonating admin's id, when the verified token is
    /// an impersonation token (P1-010): `Some` exactly when the
    /// token's claims carry `impersonator_id`. `None` on a normal
    /// login token and on every unverified context. Handlers
    /// that act for the user read `user`; the admin guards
    /// reject any context where this is `Some` (an impersonator
    /// cannot reach admin endpoints — P1-011 tests that).
    pub impersonator_id: Option<uuid::Uuid>,
}

/// The web session cookie name (provisional — Phase 1 confirms it;
/// see the module docs).
pub const REFRESH_COOKIE: &str = "detag_refresh";

impl AuthContext {
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
            impersonator_id: None,
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
/// adding a cookie crate for one lookup.
fn parse_session_cookie(headers: &HeaderMap) -> Option<Credential> {
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

/// The Bearer verification (P1-005): the extractor takes
/// [`AppState`] (the pool + the JWT secret) and rejects with a
/// problem+json 401 — never an `Ok` unverified context. A
/// handler that declares `AuthContext` therefore only ever
/// runs for a verified caller; the `user` is `Some` exactly
/// when `verified` is true. API keys and session cookies
/// reject with `PF-1001` until their tickets land (P1-016).
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
            Credential::ApiKey(_) | Credential::Session(_) => {
                return Err(reject(AppError::Unauthenticated));
            }
            Credential::Bearer(token) => token,
        };
        // The JWT verifies first (no DB touch for a bad token):
        // an invalid signature or malformed token is PF-1010, an
        // expired one PF-1011.
        let claims = match verify_access_token(&state.settings.jwt_secret, &token) {
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
        Ok(Self {
            credential: Credential::Bearer(token),
            role: Role::from_user_role(&user.role),
            verified: true,
            user: Some(user),
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
        assert!(!ctx.verified, "nothing is verified in Phase 0");
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
    /// integration tests (`tests/profile.rs`), not here.
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
            (Some(format!("Bearer {API_KEY}")), "PF-1001"),
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
}
