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
//! row and the cookie, nothing more. A login DOES cancel a
//! pending account deletion (P1-007): the grace ends at the
//! login, in the same transaction as the new session. A login
//! does NOT work for a suspended account (P1-008): after a
//! correct password the service returns 403 `PF-1003`
//! (`account_suspended`) — a wrong password is still the
//! generic 401, so probing an address learns nothing new.

use chrono::{Duration, Utc};
use detag_core::{
    checksum::sha256_hex,
    db::PoolConfig,
    error::FieldError,
    jwt::{issue_access_token, ACCESS_TOKEN_TTL_SECS, REFRESH_TOKEN_TTL_SECS},
    lockout::LOCKOUT_THRESHOLD,
    password::{verify_password, PasswordError},
    redis::ratelimit::RateLimit,
    repos::{
        audit_logs::{self, NewAuditLog},
        email_outbox::{self, NewOutboxMail},
        login_attempts::{self, LockScope},
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

use super::deletion::cancel_deletion_in_tx;
use crate::rate_limit::RateLimitScope;
use crate::AppState;

/// The refresh cookie (web session, httpOnly). The name is
/// P1-003's confirmation of the P0-070 provisional name: the
/// login handler sets it, the `AuthContext` extractor reads
/// it. P1-016 confirms the final cookie posture (Secure /
/// SameSite / Path=/v1/auth — the cookie is refresh-only, not
/// an access credential; see `auth.rs`); this ticket sets
/// `HttpOnly; Secure; SameSite=Lax; Path=/v1/auth;
/// Max-Age=2592000` (30d, matching the session).
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

/// The outbox template for the lockout notification mail
/// (P1-004): queued once per lock episode, when the account
/// first locks. The mailer is not in Phase 1 yet (see
/// `docs/03-ARCHITECTURE/03-api-spec.md` — the register flow
/// has the same pending note), so the mail is only queued; the
/// queue row is the evidence the mailer will send.
pub const LOCKOUT_NOTICE_TEMPLATE: &str = "lockout-notice";

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
/// failure for a bad password: unknown email and wrong password
/// are the same error, by design (no enumeration). `Locked` is
/// the lockout: the account (or the client IP) is temporarily
/// locked after too many failures — 423 `PF-1008`, with
/// `Retry-After` carrying the seconds until the lock lifts.
#[derive(Debug, Error)]
pub enum LoginError {
    #[error("invalid email or password")]
    Invalid,
    #[error("account suspended")]
    Suspended,
    #[error("account locked")]
    Locked {
        /// Seconds until the lock lifts (the later of the
        /// account and IP locks — the login is usable again
        /// when both have expired).
        retry_after_secs: u64,
    },
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
/// [`AppState`](crate::AppState)). `client_ip` is the abuse
/// identity for the per-IP lockout counter: the XFF-first
/// address (see `crate::rate_limit::client_ip_from`), or
/// `"unknown"` when neither source had an address — the IP
/// counter is skipped for `"unknown"` (there is no IP
/// identity to lock).
pub async fn login_user(
    state: &AppState,
    req: &LoginRequest,
    device: &DeviceInfo,
    client_ip: &str,
) -> Result<LoginOutcome, LoginError> {
    let email = req.normalized_email();
    let email_hash = sha256_hex(email.as_bytes());
    let ip_hash = if client_ip == "unknown" {
        None
    } else {
        Some(sha256_hex(client_ip.as_bytes()))
    };

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

    // Lockout (P1-004): the account and the client IP are
    // checked BEFORE the password verify — a locked login
    // never reaches the verify, so it cannot extend the lock
    // by hammering and its timing reveals nothing. The check
    // is a plain read; the record happens on failure below.
    // A locked attempt is not audited (no attempt row, no
    // failure row): an attacker hammering a locked account
    // must not flood the audit log — the lock itself was
    // audited when it was set (`login.locked`).
    let now = Utc::now();
    let mut locked_for: Option<i64> = None;
    for (scope, key) in [
        (LockScope::Account, email_hash.as_str()),
        (LockScope::Ip, ip_hash.as_deref().unwrap_or_default()),
    ] {
        if scope == LockScope::Ip && ip_hash.is_none() {
            continue;
        }
        let row = login_attempts::get(&state.pool, scope, key)
            .await
            .map_err(|err| {
                tracing::error!(error = ?err, "login: lockout lookup failed");
                LoginError::Internal
            })?;
        if let Some(row) = row {
            if let Some(until) = row.locked_until {
                if until > now {
                    let secs = (until - now).num_seconds();
                    locked_for = Some(match locked_for {
                        Some(prev) => prev.max(secs),
                        None => secs,
                    });
                }
            }
        }
    }
    if let Some(secs) = locked_for {
        return Err(LoginError::Locked {
            retry_after_secs: secs.max(1) as u64,
        });
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
        record_login_failure(
            state,
            &email_hash,
            ip_hash.as_deref(),
            None,
            "unknown_email",
        )
        .await?;
        return Err(LoginError::Invalid);
    };

    // Wrong password → generic 401. A malformed stored hash is
    // a 500 (data problem, not a wrong password).
    match verify_password(&req.password, &user.password_hash) {
        Ok(true) => {}
        Ok(false) => {
            record_login_failure(
                state,
                &email_hash,
                ip_hash.as_deref(),
                Some(user.id),
                "wrong_password",
            )
            .await?;
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

    // A suspended account cannot log in (P1-008): the password
    // verify ran FIRST (a wrong password is still the generic
    // 401, so probing an address learns nothing new), and only
    // a correct password reaches this 403. The attempt is
    // audited (`login.failure`, reason `suspended`, internal
    // only — a single insert, no tx needed for one row) and no
    // lockout counter is recorded: the account is already out,
    // and the lockout state is the admin's to clear via
    // restore. A suspended user who is ALSO locked gets the
    // 423 first (the lockout check above runs before the
    // verify); a deletion-pending user is NOT blocked here —
    // their login cancels the deletion (P1-007) below.
    if user.status == "suspended" {
        if let Err(err) = audit_logs::insert(
            &state.pool,
            &NewAuditLog {
                actor_user_id: Some(user.id),
                actor_client: None,
                action: "login.failure".to_string(),
                target_type: "user".to_string(),
                target_id: Some(user.id.to_string()),
                before: None,
                after: None,
                reason: Some("suspended".to_string()),
            },
        )
        .await
        {
            tracing::error!(error = ?err, "login: suspended-attempt audit failed");
            return Err(LoginError::Internal);
        }
        return Err(LoginError::Suspended);
    }

    // One transaction: reset both counters (a success clears
    // the lockout state) + the session row + the success audit.
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
        let email_hash = email_hash.clone();
        let ip_hash = ip_hash.clone();
        Box::pin(async move {
            // A success clears the lockout state for both the
            // account and the client IP (P1-004).
            login_attempts::reset(&mut *conn, LockScope::Account, &email_hash)
                .await
                .map_err(LoginTxError::Db)?;
            if let Some(ip) = &ip_hash {
                login_attempts::reset(&mut *conn, LockScope::Ip, ip)
                    .await
                    .map_err(LoginTxError::Db)?;
            }
            // A login during the deletion grace cancels the
            // deletion (P1-007): the account is active again
            // from this login on. The cancel commits with the
            // new session; a concurrent login that cancelled
            // first is not an error (already active).
            if user.status == "deletion_pending" {
                cancel_deletion_in_tx(&mut *conn, &user)
                    .await
                    .map_err(LoginTxError::Db)?;
            }
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

/// Record a failed login: both counters (account + IP — the
/// IP one skipped when the client is "unknown"), the failure
/// audit, and — on the first account lock — the lockout
/// notification mail + the `login.locked` audit. One
/// transaction: the counters, the audit rows, and the mail
/// commit or roll back together. The `reason` distinguishes
/// unknown-email from wrong-password for abuse analysis; it
/// never reaches a response body.
async fn record_login_failure(
    state: &AppState,
    email_hash: &str,
    ip_hash: Option<&str>,
    user_id: Option<Uuid>,
    reason: &str,
) -> Result<(), LoginError> {
    let now = Utc::now();
    let timeout = PoolConfig::default().transaction_timeout;
    tx::with_tx(&state.pool, timeout, |conn| {
        let email_hash = email_hash.to_string();
        let ip_hash = ip_hash.map(str::to_string);
        let reason = reason.to_string();
        Box::pin(async move {
            let (account, newly_locked) =
                login_attempts::record_failure(&mut *conn, LockScope::Account, &email_hash, now)
                    .await
                    .map_err(LoginTxError::Db)?;
            if let Some(ip) = &ip_hash {
                login_attempts::record_failure(&mut *conn, LockScope::Ip, ip, now)
                    .await
                    .map_err(LoginTxError::Db)?;
            }
            audit_logs::insert(
                &mut *conn,
                &NewAuditLog {
                    actor_user_id: user_id,
                    actor_client: None,
                    action: "login.failure".to_string(),
                    target_type: "user".to_string(),
                    target_id: user_id.map(|id| id.to_string()),
                    before: None,
                    after: Some(serde_json::json!({ "email_hash": email_hash, "reason": reason })),
                    reason: None,
                },
            )
            .await
            .map_err(LoginTxError::Db)?;
            // First lock only: the count just reached the
            // threshold, the account exists, and the mail has
            // not been queued for this lock yet. Extensions of
            // an existing lock do not re-notify — the user
            // already knows — and a later failure after a
            // window reset starts a new episode (attempts
            // returns to 1, so the next first lock notifies
            // again).
            if newly_locked && i64::from(account.attempts) == LOCKOUT_THRESHOLD {
                if let Some(id) = user_id {
                    email_outbox::enqueue(
                        &mut *conn,
                        &NewOutboxMail {
                            to_hash: email_hash.clone(),
                            template: LOCKOUT_NOTICE_TEMPLATE.to_string(),
                            payload: serde_json::json!({ "user_id": id }),
                            scheduled_at: None,
                        },
                    )
                    .await
                    .map_err(LoginTxError::Db)?;
                    login_attempts::mark_notified(&mut *conn, LockScope::Account, &email_hash, now)
                        .await
                        .map_err(LoginTxError::Db)?;
                    audit_logs::insert(
                        &mut *conn,
                        &NewAuditLog {
                            actor_user_id: Some(id),
                            actor_client: None,
                            action: "login.locked".to_string(),
                            target_type: "user".to_string(),
                            target_id: Some(id.to_string()),
                            before: None,
                            after: Some(
                                serde_json::json!({ "locked_until": account.locked_until }),
                            ),
                            reason: None,
                        },
                    )
                    .await
                    .map_err(LoginTxError::Db)?;
                }
            }
            Ok(())
        })
    })
    .await
    .map_err(|err| match err {
        TxError::Inner(LoginTxError::Db(db_err)) => {
            tracing::error!(error = ?db_err, "login: failure transaction failed (db)");
            LoginError::Internal
        }
        other => {
            tracing::error!(error = ?other, "login: failure transaction failed");
            LoginError::Internal
        }
    })
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
