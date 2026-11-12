//! Auth context extractor (P0-070, FR-A-02 skeleton; Phase 1
//! fills in verification).
//!
//! [`AuthContext`] is the one type every handler takes to learn
//! who the caller is: which credential was presented (Bearer JWT,
//! API key, or web session cookie), and the caller's [`Role`].
//! It is an axum `FromRequestParts` extractor, so a handler
//! declares it as a parameter and the framework runs it before
//! the handler body.
//!
//! # Phase 0 status — read this before using it
//!
//! NOTHING IS VERIFIED YET. The extractor parses the credential
//! and classifies it, but it does not check a signature, look up
//! an API key, or load a session: [`AuthContext::verified`] is
//! always `false` and [`AuthContext::role`] is always
//! [`Role::Anonymous`]. Handlers MUST NOT authorize on this
//! context in Phase 0 — and today none do: every `/v1` path is
//! still the shared stub returning 501 `PF-5004` (P0-064), so the
//! "unimplemented paths return 501" half of the spec is already
//! true and stays true until Phase 1 replaces the stubs with
//! real handlers AND this extractor with real verification.
//!
//! Phase 1 changes, in this file only:
//! - no credential → 401 `PF-1001` (today: context with
//!   [`Credential::None`], `verified: false`);
//! - a Bearer token that is not a valid JWT → 401 `PF-1010`;
//!   an expired one → 401 `PF-1011`;
//! - an unknown/revoked API key or session → 401 `PF-1001`;
//! - a verified credential → `verified: true` and the real
//!   [`Role`] from the user/session record.
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
};

use crate::request_id::RequestId;

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
/// subset). Phase 0 only ever yields [`Role::Anonymous`].
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

/// Who the caller is — as far as Phase 0 knows.
///
/// The struct is deliberately plain data: handlers read
/// [`AuthContext::credential`], [`AuthContext::role`], and
/// [`AuthContext::verified`]. Nothing here is proof of identity
/// until Phase 1 sets `verified` from a real check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthContext {
    /// The presented credential (or [`Credential::None`]).
    pub credential: Credential,
    /// The caller's role. Always [`Role::Anonymous`] in Phase 0.
    pub role: Role,
    /// Whether the credential was verified. Always `false` in
    /// Phase 0 — see the module docs.
    pub verified: bool,
}

/// The web session cookie name (provisional — Phase 1 confirms it;
/// see the module docs).
pub const REFRESH_COOKIE: &str = "detag_refresh";

impl AuthContext {
    /// Parse the credential from request headers. Pure (no I/O,
    /// no verification) — unit-tested here; the extractor below
    /// calls it.
    pub fn from_headers(headers: &HeaderMap) -> Self {
        let credential = parse_bearer(headers)
            .or_else(|| parse_session_cookie(headers))
            .unwrap_or(Credential::None);
        // Phase 0: parsed, never verified.
        Self {
            credential,
            role: Role::Anonymous,
            verified: false,
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

impl<S> FromRequestParts<S> for AuthContext
where
    S: Send + Sync,
{
    /// Phase 0 never rejects: the context is parsed but
    /// unverified, and the handler (a 501 stub today, real logic
    /// in Phase 1) decides what to do with it. Phase 1 replaces
    /// this `Ok` with the 401 rejections documented above.
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        // Touch the request id so a future rejection can echo it
        // (and so the extractor stays honest about what it read).
        let _ = parts.extensions.get::<RequestId>();
        Ok(Self::from_headers(&parts.headers))
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

    /// The extractor end to end: a probe handler that echoes the
    /// parsed context runs for every credential shape — the
    /// extractor never rejects in Phase 0 (the 501 lives in the
    /// real handlers, which are stubs until Phase 1).
    #[tokio::test]
    async fn extractor_runs_and_echoes_the_parsed_context() {
        async fn probe(ctx: AuthContext) -> impl IntoResponse {
            Json(serde_json::json!({
                "kind": ctx.credential.kind(),
                "role": format!("{:?}", ctx.role),
                "verified": ctx.verified,
            }))
        }
        let app = Router::new().route("/probe", get(probe));

        for (headers, want_kind) in [
            (format!("Bearer {JWT}"), "bearer"),
            (format!("Bearer {API_KEY}"), "api-key"),
            ("Bearer ".to_string(), "none"),
        ] {
            let req = Request::builder()
                .uri("/probe")
                .header("authorization", headers)
                .body(Body::empty())
                .unwrap();
            let res = app.clone().oneshot(req).await.expect("oneshot probe");
            assert_eq!(res.status(), 200);
            let body = axum::body::to_bytes(res.into_body(), usize::MAX)
                .await
                .expect("read body");
            let json: serde_json::Value =
                serde_json::from_slice(&body).expect("probe body is JSON");
            assert_eq!(json["kind"], want_kind);
            assert_eq!(json["role"], "Anonymous");
            assert_eq!(json["verified"], false);
        }

        // Session cookie through the extractor too.
        let req = Request::builder()
            .uri("/probe")
            .header("cookie", "detag_refresh=refresh-token-abc")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.expect("oneshot probe");
        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .expect("read body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("probe body is JSON");
        assert_eq!(json["kind"], "session");
    }
}
