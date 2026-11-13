//! Login service (P1-003, FR-A-01): `POST /v1/auth/login`.
//!
//! Flow: normalize the email → per-account rate limit (the
//! `user` bucket, keyed by the sha256 of the email — pre-auth,
//! so there is no user id yet) → audit `login.attempt` → load
//! the user → Argon2id verify → on success one transaction
//! (session row + `login.success` audit) + issue the access
//! token. The handler sets the refresh cookie from the
//! returned raw token.
//!
//! # No enumeration — two mechanisms, both load-bearing
//!
//! 1. **One generic error.** Unknown email, wrong password,
//!    and (in the future) a suspended account all answer the
//!    same 401 `PF-1001` with the taxonomy's static detail.
//!    The status, code, title, and detail never vary by cause.
//!    The *audit* log distinguishes (`login.failure` with a
//!    `reason`) — that record is internal, never in a response.
//! 2. **Constant-time verify.** An unknown email does NOT
//!    skip the Argon2id verify: the service verifies the
//!    presented password against the state's precomputed dummy
//!    hash (`AppState::dummy_password_hash`, one hash at boot
//!    with the settings' params) and discards the result — so
//!    an unknown email costs exactly one verify, the same as a
//!    wrong password. A malformed stored hash is a data
//!    problem, not a wrong password: it maps to 500, so a
//!    corrupt row cannot be probed either.
//!
//! # Rate limits (per-IP + per-account)
//!
//! Per-IP is the middleware's job (P0-103): every `/v1`
//! request — login included — spends from the IP bucket
//! before the handler runs. Per-account is this service's
//! job: the `login_account` bucket (10 burst / 1 per min by
//! default), checked BEFORE the password verify, so a
//! brute-force against one account is bounded even from many
//! IPs. A denial is a 429 `PF-3007` with `Retry-After` —
//! rendered by the same `denied_response` as the middleware,
//! with the denying bucket's numbers. Redis down fails OPEN
//! (warn logged), the P0-103 decision: the login endpoint
//! must not go down on a cache outage. P1-004's lockout is
//! the durable, DB-backed backstop that survives a Redis
//! outage — this bucket is the cheap first line.
//!
//! # What login does NOT do
//!
//! Login does NOT require a verified email: an unverified user
//! can log in (they just cannot create jobs — PF-1009, the
//! job-create guard). Verification gates job creation, not
//! authentication. Session rotation, reuse revocation, and
//! logout are P1-014 / P1-017; this ticket creates the session
//! row and the cookie, nothing more.

use chrono::{Duration, Utc};
use detag_core::{
    checksum::sha256_hex,
    db::PoolConfig,
    error::FieldError,
    jwt::{issue_access_token, ACCESS_TOKEN_TTL_SECS, REFRESH_TOKEN_TTL_SECS},
    password::{verify_password, PasswordError},
    redis::ratelimit::RateLimit,
    repos::{
        audit_logs::{self, NewAuditLog},
        sessions::{self, NewSession},
        users::{self},
        RepoError,
    },
    tokens::generate_token,
    tx::{self, TxError},
};
use detag_validator::{validate_email, Validate, ValidationErrors};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::rate_limit::RateLimitScope;
use crate::AppState;

/// The refresh cookie (web session, httpOnly). The name is
/// P1-003's confirmation of the P0-070 provisional name: the
/// login handler sets it, the `AuthContext` extractor reads
/// it. P1-015 owns the final cookie posture (Secure /
/// SameSite); this ticket sets `HttpOnly; Secure; SameSite=Lax;
/// Path=/v1/auth; Max-Age=2592000` (30d, matching the session).
pub const REFRESH_COOKIE_NAME: &str = "detag_refresh";

/// The login request body (api-spec: email, password → 200
/// access token + refresh cookie).
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct LoginRequest {
    /// The email address (trimmed + lowercased before lookup).
    pub email: String,
    /// The password (never trimmed, never logged, never stored).
    pub password: String,
}

impl Validate for LoginRequest {
    fn validate(&self) -> Result<(), ValidationErrors> {
        let mut errors = ValidationErrors::default();
        if let Err(err) = validate_email("email", &self.normalized_email()) {
            errors.push(err);
        }
        if self.password.is_empty() {
            errors.push(FieldError::new("password", "required", "is required"));
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

impl LoginRequest {
    /// The email as looked up: trimmed, lowercased.
    pub fn normalized_email(&self) -> String {
        self.email.trim().to_lowercase()
    }
}

/// The login response body: the access token + its lifetime.
/// The refresh token is NOT in the body — it travels as the
/// httpOnly `detag_refresh` cookie (set by the handler).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct LoginResponse {
    /// The HS256 access token (15 min).
    pub access_token: String,
    /// Always `"Bearer"`.
    pub token_type: String,
    /// Seconds until the access token expires (900).
    pub expires_in: i64,
}

/// The device the session was created from: the user-agent
/// string (if sent) and the client IP (socket address, as the
/// handler saw it — XFF is not used here: it is spoofable,
/// and this is the session's device record, not a limit).
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub user_agent: Option<String>,
    pub ip: String,
}

/// What a successful login returns: the response body plus
/// the raw refresh token (the handler sets it as the cookie).
/// The raw token must never be logged or stored — only its
/// sha256 lives in the session row.
#[derive(Debug)]
pub struct LoginOutcome {
    pub response: LoginResponse,
    pub refresh_token: String,
}

/// What login can fail with. `Invalid` is the ONLY client
/// failure: unknown email and wrong password are the same
/// error, by design (no enumeration).
#[derive(Debug, Error)]
pub enum LoginError {
    #[error("invalid email or password")]
    Invalid,
    #[error("too many login attempts for this account")]
    RateLimited(RateLimit),
    #[error("login failed")]
    Internal,
}

/// The transaction closure's error: small on purpose — it must
/// not recurse into [`LoginError`].
#[derive(Debug)]
enum LoginTxError {
    Db(RepoError),
}

/// Log a user in. `state` carries the pool, settings, redis,
/// rate-limit policy, and the dummy password hash (see
/// [`AppState`](crate::AppState)).
pub async fn login_user(
    state: &AppState,
    req: &LoginRequest,
    device: &DeviceInfo,
) -> Result<LoginOutcome, LoginError> {
    let email = req.normalized_email();
    let email_hash = sha256_hex(email.as_bytes());

    // Per-account rate limit, BEFORE the password verify (so a
    // brute-force against one account is bounded even from many
    // IPs). Fail open on a Redis error (P0-103): warn and
    // continue — the login endpoint must not go down on a cache
    // outage; P1-004's lockout is the durable backstop.
    let account_check = state
        .rate_limit
        .login_account
        .check(&state.redis, RateLimitScope::User.key(), &email_hash, 1.0)
        .await;
    match account_check {
        Ok(result) if !result.allowed => {
            return Err(LoginError::RateLimited(result));
        }
        Ok(_) => {}
        Err(err) => {
            tracing::warn!(scope = "login-account", error = %err, "rate limiter failing open: redis unavailable");
        }
    }

    // The attempt is audited outside the transaction: it
    // survives a rollback, like register.attempt (P1-001).
    audit_logs::insert(
        &state.pool,
        &NewAuditLog {
            actor_user_id: None,
            actor_client: None,
            action: "login.attempt".to_string(),
            target_type: "user".to_string(),
            target_id: None,
            before: None,
            after: Some(serde_json::json!({ "email_hash": email_hash })),
            reason: None,
        },
    )
    .await
    .map_err(|err| {
        tracing::error!(error = ?err, "login: attempt audit failed");
        LoginError::Internal
    })?;

    // Load the user. Unknown email → verify against the dummy
    // hash (constant time) and fail generically. The verify
    // result is discarded: the answer is Invalid either way.
    let user = match users::by_email(&state.pool, &email).await {
        Ok(user) => user,
        Err(err) => {
            tracing::error!(error = ?err, "login: user lookup failed");
            return Err(LoginError::Internal);
        }
    };
    let Some(user) = user else {
        let _ignored = verify_password(&req.password, &state.dummy_password_hash);
        audit_failure(&state.pool, &email_hash, "unknown_email").await?;
        return Err(LoginError::Invalid);
    };

    // Wrong password → generic 401. A malformed stored hash is
    // a 500 (data problem, not a wrong password).
    match verify_password(&req.password, &user.password_hash) {
        Ok(true) => {}
        Ok(false) => {
            audit_failure(&state.pool, &email_hash, "wrong_password").await?;
            return Err(LoginError::Invalid);
        }
        Err(PasswordError::MalformedHash) => {
            tracing::error!(user_id = %user.id, "login: stored password hash is malformed");
            return Err(LoginError::Internal);
        }
        Err(err) => {
            tracing::error!(error = ?err, "login: password verify failed");
            return Err(LoginError::Internal);
        }
    }

    // One transaction: the session row + the success audit.
    // The refresh token is generated here, inside the service
    // (not the handler): it must exist before the row is
    // written, and only its hash is stored.
    let refresh_token = generate_token();
    let refresh_hash = sha256_hex(refresh_token.as_bytes());
    let family_id = Uuid::now_v7();
    let device_json = serde_json::json!({
        "ua": device.user_agent,
        "ip_hash": sha256_hex(device.ip.as_bytes()),
    });
    let expires_at = Utc::now() + Duration::seconds(REFRESH_TOKEN_TTL_SECS);
    let timeout = PoolConfig::default().transaction_timeout;
    tx::with_tx(&state.pool, timeout, |conn| {
        let refresh_hash = refresh_hash.clone();
        let device_json = device_json.clone();
        let user = user.clone();
        Box::pin(async move {
            let session = sessions::create(
                &mut *conn,
                &NewSession {
                    user_id: user.id,
                    refresh_hash,
                    family_id,
                    device: device_json,
                    expires_at,
                },
            )
            .await
            .map_err(LoginTxError::Db)?;
            audit_logs::insert(
                &mut *conn,
                &NewAuditLog {
                    actor_user_id: Some(user.id),
                    actor_client: None,
                    action: "login.success".to_string(),
                    target_type: "user".to_string(),
                    target_id: Some(user.id.to_string()),
                    before: None,
                    after: Some(serde_json::json!({ "session_id": session.id })),
                    reason: None,
                },
            )
            .await
            .map_err(LoginTxError::Db)?;
            Ok(())
        })
    })
    .await
    .map_err(|err| match err {
        TxError::Inner(LoginTxError::Db(db_err)) => {
            tracing::error!(error = ?db_err, "login: session transaction failed (db)");
            LoginError::Internal
        }
        other => {
            tracing::error!(error = ?other, "login: session transaction failed");
            LoginError::Internal
        }
    })?;

    let access_token = issue_access_token(
        &state.settings.jwt_secret,
        user.id,
        &user.role,
        ACCESS_TOKEN_TTL_SECS,
    )
    .map_err(|err| {
        tracing::error!(error = ?err, "login: access-token issue failed");
        LoginError::Internal
    })?;

    Ok(LoginOutcome {
        response: LoginResponse {
            access_token,
            token_type: "Bearer".to_string(),
            expires_in: ACCESS_TOKEN_TTL_SECS,
        },
        refresh_token,
    })
}

/// Audit a failed login (internal record only — the `reason`
/// distinguishes unknown-email from wrong-password for abuse
/// analysis; it never reaches a response body).
async fn audit_failure(
    pool: &detag_core::db::DbPool,
    email_hash: &str,
    reason: &str,
) -> Result<(), LoginError> {
    audit_logs::insert(
        pool,
        &NewAuditLog {
            actor_user_id: None,
            actor_client: None,
            action: "login.failure".to_string(),
            target_type: "user".to_string(),
            target_id: None,
            before: None,
            after: Some(serde_json::json!({ "email_hash": email_hash, "reason": reason })),
            reason: None,
        },
    )
    .await
    .map_err(|err| {
        tracing::error!(error = ?err, "login: failure audit failed");
        LoginError::Internal
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_request_requires_email_shape_and_password() {
        let req = LoginRequest {
            email: "not-an-email".to_string(),
            password: "x".to_string(),
        };
        let err = req.validate().expect_err("bad email must fail");
        assert_eq!(err.0[0].field, "email");
        assert_eq!(err.0[0].code, "invalid_email");
        let req = LoginRequest {
            email: "a@example.com".to_string(),
            password: String::new(),
        };
        let err = req.validate().expect_err("empty password must fail");
        assert_eq!(err.0[0].field, "password");
        assert_eq!(err.0[0].code, "required");
        let req = LoginRequest {
            email: "  A@Example.com ".to_string(),
            password: "correct horse battery staple".to_string(),
        };
        assert!(req.validate().is_ok());
        assert_eq!(req.normalized_email(), "a@example.com");
    }
}
