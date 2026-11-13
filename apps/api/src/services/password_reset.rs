//! Forgot / reset password flow (P1-025, FR-A-01).
//!
//! Two endpoints, one rule above all: the forgot request
//! NEVER reveals whether the email has an account.
//!
//! - `POST /v1/auth/forgot-password` — always 204, for a
//!   known email AND an unknown one (byte-identical
//!   responses; the only observable difference is the
//!   rate-limit 429, which the per-email bucket produces
//!   identically for both). A known email mints a reset
//!   token + queues the reset mail + audits
//!   `password.reset_requested`; an unknown email audits
//!   the same action with only the email hash (no user row
//!   to point at) and mints nothing. The reset-account
//!   bucket (10 burst / 1 per min, P1-020) is spent BEFORE
//!   the lookup, keyed by the sha256 of the normalized
//!   email — so the bucket trips identically for known and
//!   unknown addresses, and a denied request does no work
//!   at all. A Redis error fails OPEN (P0-103): warn and
//!   continue.
//! - `POST /v1/auth/reset-password` — consumes the token
//!   and sets the new password. Unknown / used /
//!   wrong-purpose token → 401 `PF-1010`; expired token →
//!   401 `PF-1011`. The token is single-use (the consume
//!   is atomic: `used_at IS NULL AND expires_at > now()`),
//!   30 minutes (`RESET_TOKEN_TTL_SECS`), hashed
//!   (sha256, the raw token lives only in the outbox
//!   payload + the email), purpose `password_reset` (the
//!   migration CHECK admits it; a registration or
//!   email-change token is `InvalidToken` here, never a
//!   reset). On success the password is re-hashed with
//!   the current Argon2 params, the lockout counter for
//!   the account is cleared (P1-004's hook — a reset
//!   un-locks the account), and EVERY session is revoked
//!   via the shared service
//!   ([`revoke_sessions_on_password_change`](super::password::revoke_sessions_on_password_change),
//!   the reset case: no current session) — which also
//!   bumps the revocation epoch, so pre-reset access
//!   tokens die at the extractor, and queues the
//!   `new-login-digest` mail.
//!
//! Ordering (two transactions, on purpose): the token
//! consume + password write + counter reset commit
//! together FIRST (a consumed token with an unchanged
//! password — or a changed password with a live token —
//! cannot exist); the session revocation runs second, in
//! its own transaction. If the revocation fails, the
//! password is already changed and the handler answers
//! 500 — the caller requests a fresh reset token (the
//! forgot flow still works; the account is not locked).
//! The reverse order would be worse: revoking first and
//! then failing the password write would sign the user
//! out everywhere while leaving the old password in
//! place.
//!
//! One live token per user (the P1-006 rule, applied
//! across purposes): minting a reset token deletes the
//! user's previous UNUSED tokens — a live verification or
//! email-change token dies with it. The pending intent
//! survives (it lives on the user row).

use crate::rate_limit::RateLimitScope;
use crate::AppState;
use detag_core::{
    checksum::sha256_hex,
    db::PoolConfig,
    password::{hash_password, Argon2Params},
    redis::ratelimit::RateLimit,
    repos::{
        audit_logs::{self, NewAuditLog},
        email_outbox::{self, NewOutboxMail},
        email_verification_tokens::{self, PURPOSE_PASSWORD_RESET},
        login_attempts::{self, LockScope},
        users::{self},
        RepoError,
    },
    tokens::{generate_token, hash_token},
    tx::{self, TxError},
};
use detag_validator::{validate_email, validate_password, Validate, ValidationErrors};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use utoipa::ToSchema;

/// Reset-token lifetime: 30 minutes (P1-025).
pub const RESET_TOKEN_TTL_SECS: i64 = 30 * 60;

/// Outbox template for the reset mail (P1-025): payload
/// `{user_id, token}` — the mailer (P1-028/029) renders the
/// reset link. The raw token lives only here + in the email.
pub const PASSWORD_RESET_TEMPLATE: &str = "password-reset";

/// What the forgot / reset flow can fail with.
/// `InvalidToken` / `ExpiredToken` map to 401 `PF-1010` /
/// `PF-1011`; `RateLimited` to 429 `PF-3007` (the trio names
/// the reset-account bucket); `Internal` carries no detail.
#[derive(Debug, Error)]
pub enum PasswordResetError {
    #[error("invalid reset token")]
    InvalidToken,
    #[error("expired reset token")]
    ExpiredToken,
    #[error("too many reset attempts for this email")]
    RateLimited(RateLimit, u64),
    #[error("password reset failed")]
    Internal,
}

/// The transaction closure's error: small on purpose — it must
/// not recurse into [`PasswordResetError`].
#[derive(Debug)]
enum ResetTxError {
    Db(RepoError),
}

/// The forgot-password request body: the email that may (or
/// may not) have an account.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ForgotPasswordRequest {
    /// The email to (maybe) send a reset link to.
    pub email: String,
}

impl Validate for ForgotPasswordRequest {
    fn validate(&self) -> Result<(), ValidationErrors> {
        let mut errors = ValidationErrors::default();
        if let Err(err) = validate_email("email", &self.normalized_email()) {
            errors.push(err);
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

impl ForgotPasswordRequest {
    /// The email as looked up: trimmed, lowercased.
    pub fn normalized_email(&self) -> String {
        self.email.trim().to_lowercase()
    }
}

/// The reset-password request body: the token from the reset
/// link + the new password.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ResetPasswordRequest {
    /// The raw token from the reset link.
    pub token: String,
    /// The new password (min 12 chars — the P1-001 rule;
    /// strength + blocklist land with P1-026).
    pub new_password: String,
}

impl Validate for ResetPasswordRequest {
    fn validate(&self) -> Result<(), ValidationErrors> {
        let mut errors = ValidationErrors::default();
        if self.token.trim().is_empty() {
            errors.push(detag_core::error::FieldError::new(
                "token",
                "required",
                "is required",
            ));
        }
        if let Err(err) = validate_password("new_password", &self.new_password) {
            errors.push(err);
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

/// Request a password reset. Always `Ok` — the handler
/// answers 204 whether or not the email has an account (no
/// enumeration). The rate-limit denial is the only
/// non-204 outcome, and it is identical for known and
/// unknown addresses.
pub async fn request_password_reset(
    state: &AppState,
    req: &ForgotPasswordRequest,
) -> Result<(), PasswordResetError> {
    let email = req.normalized_email();

    // Per-email rate limit (P1-020), FIRST — before the
    // lookup, so it trips identically for known and unknown
    // addresses, and a denied request does no work at all.
    // Keyed by the sha256 of the normalized email
    // (pre-auth, so there is no user id yet). Fail open on
    // a Redis error (P0-103): warn and continue.
    let email_hash = sha256_hex(email.as_bytes());
    let bucket_check = state
        .rate_limit
        .reset_account
        .check(
            &state.redis,
            RateLimitScope::ResetAccount.key(),
            &email_hash,
            1.0,
        )
        .await;
    match bucket_check {
        Ok(result) if !result.allowed => {
            return Err(PasswordResetError::RateLimited(
                result,
                state.rate_limit.reset_account_burst(),
            ));
        }
        Ok(_) => {}
        Err(err) => {
            tracing::warn!(scope = "reset-account", error = %err, "rate limiter failing open: redis unavailable");
        }
    }

    let user = users::by_email(&state.pool, &email).await.map_err(|err| {
        tracing::error!(error = ?err, "forgot-password: user lookup failed");
        PasswordResetError::Internal
    })?;
    let Some(user) = user else {
        // Unknown email: audit with the hash only (no user
        // row, no PII — the register.attempt pattern) and
        // mint nothing. The 204 the handler answers is
        // identical to the known-email one.
        audit_logs::insert(
            &state.pool,
            &NewAuditLog {
                actor_user_id: None,
                actor_client: None,
                action: "password.reset_requested".to_string(),
                target_type: "user".to_string(),
                target_id: None,
                before: None,
                after: Some(serde_json::json!({ "email_hash": email_hash })),
                reason: None,
            },
        )
        .await
        .map_err(|err| {
            tracing::error!(error = ?err, "forgot-password: audit failed");
            PasswordResetError::Internal
        })?;
        return Ok(());
    };

    // Known email: one transaction — delete the user's
    // previous unused tokens (one live token per user),
    // mint the reset token, queue its mail, audit.
    let now = state.clock.now();
    let timeout = PoolConfig::default().transaction_timeout;
    tx::with_tx(&state.pool, timeout, |conn| {
        let user = user.clone();
        let email_hash = email_hash.clone();
        Box::pin(async move {
            email_verification_tokens::delete_unused_for_user(&mut *conn, user.id)
                .await
                .map_err(ResetTxError::Db)?;
            let raw = generate_token();
            let hash = hash_token(&raw);
            let expires_at = now + chrono::Duration::seconds(RESET_TOKEN_TTL_SECS);
            email_verification_tokens::mint(
                &mut *conn,
                user.id,
                &hash,
                PURPOSE_PASSWORD_RESET,
                expires_at,
            )
            .await
            .map_err(ResetTxError::Db)?;
            email_outbox::enqueue(
                &mut *conn,
                &NewOutboxMail {
                    to_hash: sha256_hex(user.email.as_bytes()),
                    template: PASSWORD_RESET_TEMPLATE.to_string(),
                    payload: serde_json::json!({ "user_id": user.id, "token": raw }),
                    scheduled_at: None,
                },
            )
            .await
            .map_err(ResetTxError::Db)?;
            audit_logs::insert(
                &mut *conn,
                &NewAuditLog {
                    actor_user_id: None,
                    actor_client: None,
                    action: "password.reset_requested".to_string(),
                    target_type: "user".to_string(),
                    target_id: Some(user.id.to_string()),
                    before: None,
                    after: Some(serde_json::json!({ "email_hash": email_hash })),
                    reason: None,
                },
            )
            .await
            .map_err(ResetTxError::Db)?;
            Ok(())
        })
    })
    .await
    .map_err(|err| match err {
        TxError::Inner(ResetTxError::Db(db_err)) => {
            tracing::error!(error = ?db_err, "forgot-password: transaction failed (db)");
            PasswordResetError::Internal
        }
        other => {
            tracing::error!(error = ?other, "forgot-password: transaction failed");
            PasswordResetError::Internal
        }
    })
}

/// Reset the password with a token from the reset link.
/// Consumes the token (single-use), writes the new hash,
/// clears the account's lockout counter, revokes every
/// session + bumps the epoch, and audits `password.reset`.
pub async fn reset_password(
    state: &AppState,
    req: &ResetPasswordRequest,
) -> Result<(), PasswordResetError> {
    // Hash before any DB write (CPU work, no transaction
    // held) — the register pattern.
    let params = Argon2Params::from_settings(&state.settings);
    let new_hash = hash_password(&req.new_password, &params).map_err(|err| {
        tracing::error!(error = ?err, "reset-password: password hashing failed");
        PasswordResetError::Internal
    })?;

    let hash = hash_token(req.token.trim());
    let token = email_verification_tokens::by_hash(&state.pool, &hash)
        .await
        .map_err(|err| {
            tracing::error!(error = ?err, "reset-password: token lookup failed");
            PasswordResetError::Internal
        })?;
    let Some(token) = token else {
        return Err(PasswordResetError::InvalidToken);
    };
    if token.used_at.is_some() {
        return Err(PasswordResetError::InvalidToken);
    }
    if token.purpose != PURPOSE_PASSWORD_RESET {
        // A registration or email-change token is not a
        // reset token (purpose-separated, P1-006's lesson) —
        // and it is NOT consumed here, so it still works
        // for its own flow afterwards.
        return Err(PasswordResetError::InvalidToken);
    }
    if token.expires_at <= state.clock.now() {
        return Err(PasswordResetError::ExpiredToken);
    }
    let user_id = token.user_id;
    let user = users::by_id(&state.pool, user_id).await.map_err(|err| {
        tracing::error!(error = ?err, "reset-password: user lookup failed");
        PasswordResetError::Internal
    })?;
    let Some(user) = user else {
        // The token row cascades with its user, so this is
        // unreachable in practice — a dead token either way.
        return Err(PasswordResetError::InvalidToken);
    };
    let email_hash = sha256_hex(user.email.as_bytes());

    // Tx 1: consume the token + write the new hash + clear
    // the account's lockout counter + audit — commit or
    // roll back together.
    let timeout = PoolConfig::default().transaction_timeout;
    tx::with_tx(&state.pool, timeout, |conn| {
        let hash = hash.clone();
        let new_hash = new_hash.clone();
        let email_hash = email_hash.clone();
        let user = user.clone();
        Box::pin(async move {
            let consumed = email_verification_tokens::consume(&mut *conn, &hash)
                .await
                .map_err(ResetTxError::Db)?;
            if !consumed {
                // Consumed (or expired) between the read and
                // the consume — the caller maps it to
                // InvalidToken, never to a distinct "already
                // used" error.
                return Err(ResetTxError::Db(RepoError::NotFound));
            }
            users::set_password_hash(&mut *conn, user.id, &new_hash)
                .await
                .map_err(ResetTxError::Db)?;
            // A reset un-locks the account (P1-004's hook).
            login_attempts::reset(&mut *conn, LockScope::Account, &email_hash)
                .await
                .map_err(ResetTxError::Db)?;
            audit_logs::insert(
                &mut *conn,
                &NewAuditLog {
                    actor_user_id: Some(user.id),
                    actor_client: None,
                    action: "password.reset".to_string(),
                    target_type: "user".to_string(),
                    target_id: Some(user.id.to_string()),
                    before: None,
                    after: None,
                    reason: None,
                },
            )
            .await
            .map_err(ResetTxError::Db)?;
            Ok(())
        })
    })
    .await
    .map_err(|err| match err {
        TxError::Inner(ResetTxError::Db(RepoError::NotFound)) => PasswordResetError::InvalidToken,
        TxError::Inner(ResetTxError::Db(db_err)) => {
            tracing::error!(error = ?db_err, "reset-password: transaction failed (db)");
            PasswordResetError::Internal
        }
        other => {
            tracing::error!(error = ?other, "reset-password: transaction failed");
            PasswordResetError::Internal
        }
    })?;

    // Tx 2: revoke every session + bump the epoch (the
    // shared P1-018 service — its own transaction, audit,
    // and digest mail). If this fails the password is
    // already changed; the handler answers 500 and the
    // caller requests a fresh token (see the module docs).
    super::password::revoke_sessions_on_password_change(state, user.id, None)
        .await
        .map_err(|err| {
            tracing::error!(error = ?err, "reset-password: session revocation failed after password change");
            PasswordResetError::Internal
        })?;
    Ok(())
}
