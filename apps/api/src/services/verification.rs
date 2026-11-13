//! Verification service (P1-002, FR-A-01): the email
//! verification flow.
//!
//! Three operations, one token shape (single-use, 24h, stored
//! as sha256 — see `detag_core::tokens`):
//!
//! - [`issue_verification`] — mint a token for a user and queue
//!   its mail. Called inside the caller's transaction (register
//!   and resend both own one): it deletes the user's previous
//!   unused tokens, mints, and enqueues the outbox row with
//!   payload `{user_id, token}`. The raw token is in the outbox
//!   payload because the mail drain needs it to build the link —
//!   the token is a single-use 24h credential, and its presence
//!   there is equivalent to its presence in the email itself
//!   (cleartext in the mailbox). It must never appear in logs
//!   or error bodies; the database holds only its hash.
//! - [`verify_email`] — consume a token: hash → lookup →
//!   expiry/used checks → atomic consume → set
//!   `email_verified_at` → audit. Unknown, used, and expired
//!   tokens are deliberately indistinguishable in the *used*
//!   case only: an expired token is `ExpiredToken` (PF-1011),
//!   an unknown or already-used token is `InvalidToken`
//!   (PF-1010) — a used token must not tell the caller it was
//!   once valid.
//! - [`resend_verification`] — mint a fresh token + mail for an
//!   unverified user, at most 3 per hour. The count is the
//!   outbox rows for this user + template in the last hour —
//!   every mint enqueues exactly one row in the same
//!   transaction, so the outbox count IS the mint count, and
//!   unlike the token table it survives: minting deletes the
//!   user's previous unused tokens (one live token per user),
//!   so counting token rows would under-count and the limit
//!   would never bind (caught by the rate-limit test, which
//!   saw a 4th mail queued).
//!   (no enumeration, mirroring forgot-password): unknown
//!   emails, already-verified users, and over-limit resends all
//!   look identical from outside. The rate limit is enforced
//!   silently — the mail just does not come — and is proven by
//!   the token-table count in tests.
//!
//! The job-creation gate ([`require_verified`]) lives here too:
//! `EmailNotVerified` (PF-1009, 403) for unverified users. The
//! job-create handler (Phase 2) calls it first; until then the
//! guard is proven by its own tests (verified passes,
//! unverified 403s), not by a live job endpoint.

use chrono::{Duration, Utc};
use detag_core::{
    checksum::sha256_hex,
    db::DbPool,
    error::AppError,
    repos::{
        audit_logs::{self, NewAuditLog},
        email_outbox::{self, NewOutboxMail},
        email_verification_tokens::{self},
        users::{self, User},
        RepoError,
    },
    tokens::{generate_token, hash_token, VERIFICATION_TOKEN_TTL_SECS},
    tx::{self, TxError},
};
use detag_validator::{validate_email, Validate, ValidationErrors};
use serde::Deserialize;
use sqlx::PgConnection;
use thiserror::Error;
use utoipa::ToSchema;

/// Outbox template for the verification mail (shared with the
/// register service — P1-001 enqueued it, P1-002 fills it).
pub const VERIFY_EMAIL_TEMPLATE: &str = "verify-email";

/// Resend limit: at most this many verification tokens minted
/// per user per hour (P1-002 spec).
pub const RESEND_MAX_PER_HOUR: i64 = 3;
/// The resend window in seconds.
pub const RESEND_WINDOW_SECS: i64 = 60 * 60;

/// The verify-email request body (api-spec: token → 204).
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct VerifyEmailRequest {
    /// The raw token from the verification link.
    pub token: String,
}

impl Validate for VerifyEmailRequest {
    fn validate(&self) -> Result<(), ValidationErrors> {
        let mut errors = ValidationErrors::default();
        if self.token.trim().is_empty() {
            errors.push(detag_core::error::FieldError::new(
                "token",
                "required",
                "is required",
            ));
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

/// The resend-verification request body (P1-002: no route in
/// the original spec — added by this ticket, see the handler).
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ResendVerificationRequest {
    /// The email to (maybe) re-send a verification mail to.
    pub email: String,
}

impl Validate for ResendVerificationRequest {
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

impl ResendVerificationRequest {
    /// The email as looked up: trimmed, lowercased.
    pub fn normalized_email(&self) -> String {
        self.email.trim().to_lowercase()
    }
}

/// What verification can fail with. `Internal` carries no
/// detail by design — the service logs the source (which never
/// contains PII: no email, no token, no hash) and the handler
/// renders the generic 500.
#[derive(Debug, Error)]
pub enum VerifyError {
    #[error("invalid verification token")]
    InvalidToken,
    #[error("verification token expired")]
    ExpiredToken,
    #[error("verification failed")]
    Internal,
}

/// The transaction closure's error: small on purpose — it must
/// not recurse into [`VerifyError`].
#[derive(Debug)]
enum VerifyTxError {
    /// The token was consumed (or expired) between the read
    /// and the consume — the caller maps it to `InvalidToken`.
    AlreadyConsumed,
    Db(RepoError),
}

/// Mint a verification token for `user` and queue its mail.
/// Runs inside the caller's transaction (`conn`): the caller
/// owns the atomicity (register: user + token + mail; resend:
/// token + mail + audit). Returns the RAW token — the caller
/// must not log it, store it anywhere except the outbox
/// payload, or return it to a client.
pub async fn issue_verification(conn: &mut PgConnection, user: &User) -> Result<String, RepoError> {
    email_verification_tokens::delete_unused_for_user(&mut *conn, user.id).await?;
    let raw = generate_token();
    let hash = hash_token(&raw);
    let expires_at = Utc::now() + Duration::seconds(VERIFICATION_TOKEN_TTL_SECS);
    email_verification_tokens::mint(&mut *conn, user.id, &hash, expires_at).await?;
    email_outbox::enqueue(
        &mut *conn,
        &NewOutboxMail {
            to_hash: sha256_hex(user.email.as_bytes()),
            template: VERIFY_EMAIL_TEMPLATE.to_string(),
            payload: serde_json::json!({ "user_id": user.id, "token": raw }),
            scheduled_at: None,
        },
    )
    .await?;
    Ok(raw)
}

/// Consume a verification token: mark the user verified.
/// Unknown token → `InvalidToken`; used token → `InvalidToken`
/// (indistinguishable from unknown, by design); expired token
/// → `ExpiredToken`. On success the user is returned.
pub async fn verify_email(pool: &DbPool, raw_token: &str) -> Result<User, VerifyError> {
    let hash = hash_token(raw_token.trim());
    let token = email_verification_tokens::by_hash(pool, &hash)
        .await
        .map_err(|err| {
            tracing::error!(error = ?err, "verify-email: token lookup failed");
            VerifyError::Internal
        })?;
    let Some(token) = token else {
        return Err(VerifyError::InvalidToken);
    };
    if token.used_at.is_some() {
        return Err(VerifyError::InvalidToken);
    }
    if token.expires_at <= Utc::now() {
        return Err(VerifyError::ExpiredToken);
    }
    let user_id = token.user_id;
    let timeout = detag_core::db::PoolConfig::default().transaction_timeout;
    let user = tx::with_tx(pool, timeout, |conn| {
        let hash = hash.clone();
        Box::pin(async move {
            let consumed = email_verification_tokens::consume(&mut *conn, &hash)
                .await
                .map_err(VerifyTxError::Db)?;
            if !consumed {
                return Err(VerifyTxError::AlreadyConsumed);
            }
            let user = users::set_email_verified(&mut *conn, user_id)
                .await
                .map_err(VerifyTxError::Db)?;
            audit_logs::insert(
                &mut *conn,
                &NewAuditLog {
                    actor_user_id: Some(user.id),
                    actor_client: None,
                    action: "verify-email.success".to_string(),
                    target_type: "user".to_string(),
                    target_id: Some(user.id.to_string()),
                    before: None,
                    after: Some(serde_json::json!({ "email_verified": true })),
                    reason: None,
                },
            )
            .await
            .map_err(VerifyTxError::Db)?;
            Ok(user)
        })
    })
    .await
    .map_err(|err| match err {
        TxError::Inner(VerifyTxError::AlreadyConsumed) => VerifyError::InvalidToken,
        TxError::Inner(VerifyTxError::Db(db_err)) => {
            tracing::error!(error = ?db_err, "verify-email: transaction failed (db)");
            VerifyError::Internal
        }
        other => {
            tracing::error!(error = ?other, "verify-email: transaction failed");
            VerifyError::Internal
        }
    })?;
    Ok(user)
}

/// What a resend attempt did. The handler answers 204 for all
/// three — the outcome is for tests and logs, never for the
/// response body (no enumeration).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResendOutcome {
    /// A fresh token was minted and its mail queued.
    Sent,
    /// No mail was queued: unknown email, already verified, or
    /// over the hourly limit. Indistinguishable from outside.
    Skipped,
}

/// Re-send a verification mail to `email` (normalized by the
/// caller). 204-always semantics: the outcome is returned, the
/// handler never varies the response by it.
pub async fn resend_verification(pool: &DbPool, email: &str) -> Result<ResendOutcome, VerifyError> {
    let user = users::by_email(pool, email).await.map_err(|err| {
        tracing::error!(error = ?err, "resend-verification: user lookup failed");
        VerifyError::Internal
    })?;
    let Some(user) = user else {
        return Ok(ResendOutcome::Skipped);
    };
    if user.email_verified_at.is_some() {
        return Ok(ResendOutcome::Skipped);
    }
    let recent = email_outbox::count_recent_for_user(
        pool,
        VERIFY_EMAIL_TEMPLATE,
        user.id,
        RESEND_WINDOW_SECS,
    )
    .await
    .map_err(|err| {
        tracing::error!(error = ?err, "resend-verification: rate-limit count failed");
        VerifyError::Internal
    })?;
    if recent >= RESEND_MAX_PER_HOUR {
        return Ok(ResendOutcome::Skipped);
    }
    let timeout = detag_core::db::PoolConfig::default().transaction_timeout;
    tx::with_tx(pool, timeout, |conn| {
        let user = user.clone();
        Box::pin(async move {
            let _raw = issue_verification(&mut *conn, &user)
                .await
                .map_err(VerifyTxError::Db)?;
            audit_logs::insert(
                &mut *conn,
                &NewAuditLog {
                    actor_user_id: Some(user.id),
                    actor_client: None,
                    action: "verify-email.resend".to_string(),
                    target_type: "user".to_string(),
                    target_id: Some(user.id.to_string()),
                    before: None,
                    after: None,
                    reason: None,
                },
            )
            .await
            .map_err(VerifyTxError::Db)?;
            Ok(())
        })
    })
    .await
    .map_err(|err| match err {
        TxError::Inner(VerifyTxError::AlreadyConsumed) => {
            // Unreachable: resend mints, it never consumes.
            tracing::error!("resend-verification: unexpected AlreadyConsumed");
            VerifyError::Internal
        }
        TxError::Inner(VerifyTxError::Db(db_err)) => {
            tracing::error!(error = ?db_err, "resend-verification: transaction failed (db)");
            VerifyError::Internal
        }
        other => {
            tracing::error!(error = ?other, "resend-verification: transaction failed");
            VerifyError::Internal
        }
    })?;
    Ok(ResendOutcome::Sent)
}

/// The job-creation gate (FR-A-01, PF-1009): an unverified user
/// cannot create jobs. The Phase 2 job-create handler calls
/// this first; until then it is proven by its own tests.
/// Pure on purpose: the caller already loaded the user (the
/// auth context), so no second lookup is needed here.
pub fn require_verified(user: &User) -> Result<(), AppError> {
    if user.email_verified_at.is_some() {
        Ok(())
    } else {
        Err(AppError::EmailNotVerified)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_request_requires_a_token() {
        let req = VerifyEmailRequest {
            token: "   ".to_string(),
        };
        let err = req.validate().expect_err("blank token must fail");
        assert_eq!(err.0[0].field, "token");
        assert_eq!(err.0[0].code, "required");
        let req = VerifyEmailRequest {
            token: generate_token(),
        };
        assert!(req.validate().is_ok());
    }

    #[test]
    fn resend_request_validates_the_email_shape() {
        let req = ResendVerificationRequest {
            email: "not-an-email".to_string(),
        };
        let err = req.validate().expect_err("bad email must fail");
        assert_eq!(err.0[0].field, "email");
        assert_eq!(err.0[0].code, "invalid_email");
    }
}
