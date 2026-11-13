//! Register service (P1-001, FR-A-01): `POST /v1/auth/register`.
//!
//! Flow: normalize the email → policy checks (disposable
//! domain, ToS version — both need `Settings`, so they live
//! here, not in the DTO's structural validation) → Argon2id
//! hash (params from config) → audit `register.attempt` → one
//! transaction: create the user (recording the accepted ToS
//! version) + audit `register.success` + enqueue the
//! verification mail in the outbox. The transaction is the
//! atomicity: no user without its verification mail queued,
//! and no mail for a user that was not created.
//!
//! No enumeration, by the taxonomy's design: a duplicate email
//! is `409 PF-2011 Conflict` with the generic `conflict` body —
//! the body never says which email is taken (the taxonomy names
//! duplicate email as the Conflict example). Note the residual:
//! the *status* distinguishes duplicate (409) from new (201),
//! so a caller probing emails learns existence from the status
//! code alone. If the owner wants full indistinguishability
//! (201 for duplicates too), that is a DECISIONS.md item, not a
//! silent change here.
//!
//! Privacy (NFR-42): the audit `after` payload and the outbox
//! `to_hash` carry the email's sha256, never the email. The
//! outbox payload is `{user_id}` only — the drain resolves the
//! address from the users table at send time. The verification
//! *token* is P1-002's scope: this ticket queues the mail; P1-002
//! mints the single-use token (24h) and makes the queued mail
//! sendable.
//!
//! Concurrency: the duplicate check is the citext unique
//! constraint, not a pre-read — two concurrent registers for
//! the same email race, one wins, the other gets
//! `UniqueViolation` → 409. A `by_email` pre-check would be a
//! TOCTOU hole.

use chrono::{DateTime, Utc};
use detag_core::{
    checksum::sha256_hex,
    config::Settings,
    db::{DbPool, PoolConfig},
    error::FieldError,
    password::{hash_password, Argon2Params, PasswordError},
    repos::{
        audit_logs::{self, NewAuditLog},
        email_outbox::{self, NewOutboxMail},
        users::{self, User},
        RepoError,
    },
    tx::{self, TxError},
};
use detag_validator::{validate_email, validate_password, Validate, ValidationErrors};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use utoipa::ToSchema;
use uuid::Uuid;

/// Outbox template for the verification mail. The mailer drain
/// (E1.12) sends rows with this template; P1-002 adds the token
/// to the payload.
pub const VERIFY_EMAIL_TEMPLATE: &str = "verify-email";

/// The register request body (api-spec: email, password,
/// accept_tos_version → 201 user + verification email sent).
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct RegisterRequest {
    /// The email address (trimmed + lowercased before storage).
    pub email: String,
    /// The password (never trimmed, never logged, never stored —
    /// only its Argon2id hash is).
    pub password: String,
    /// The ToS version the user accepted; must equal the current
    /// version (`PF_TOS_CURRENT_VERSION`, default "v1").
    pub accept_tos_version: String,
}

impl RegisterRequest {
    /// The email as stored: trimmed, lowercased. The domain is
    /// lowercased too — the whole address is case-insensitive for
    /// delivery, and storing the normalized form keeps the citext
    /// column tidy (the column would match either way).
    pub fn normalized_email(&self) -> String {
        self.email.trim().to_lowercase()
    }
}

impl Validate for RegisterRequest {
    /// Structural validation only (no settings): email shape on
    /// the *normalized* email (a pasted address with surrounding
    /// spaces is a typo, not an invalid address), password length,
    /// ToS version present. The settings-dependent checks
    /// (disposable domain, ToS version match) run in
    /// [`register_user`] and produce the same 422 shape.
    fn validate(&self) -> Result<(), ValidationErrors> {
        let mut errors = ValidationErrors::default();
        if let Err(err) = validate_email("email", &self.normalized_email()) {
            errors.push(err);
        }
        if let Err(err) = validate_password("password", &self.password) {
            errors.push(err);
        }
        if self.accept_tos_version.trim().is_empty() {
            errors.push(FieldError::new(
                "accept_tos_version",
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

/// The register response: the created user, without the password
/// hash (there is no field for it — the type makes the leak
/// impossible, not just unlikely).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct UserResponse {
    pub id: Uuid,
    pub email: String,
    pub role: String,
    pub status: String,
    pub email_verified_at: Option<DateTime<Utc>>,
    pub tos_version: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<User> for UserResponse {
    fn from(user: User) -> Self {
        Self {
            id: user.id,
            email: user.email,
            role: user.role,
            status: user.status,
            email_verified_at: user.email_verified_at,
            tos_version: user.tos_version,
            created_at: user.created_at,
            updated_at: user.updated_at,
        }
    }
}

/// What registration can fail with. `Internal` carries no detail
/// by design — the service logs the source (which never contains
/// PII: no email, no password, no hash) and the handler renders
/// the generic 500.
#[derive(Debug, Error)]
pub enum RegisterError {
    #[error("email already registered")]
    Duplicate,
    #[error("registration invalid")]
    Invalid(ValidationErrors),
    #[error("registration failed")]
    Internal,
}

/// The transaction closure's error: small on purpose — it must
/// not recurse into [`RegisterError`].
#[derive(Debug)]
enum RegisterTxError {
    Duplicate,
    Db(RepoError),
}

/// True when the email's domain is on the configured disposable
/// denylist (comma-separated, case-insensitive). An empty list
/// means no denylist.
fn is_disposable(email: &str, denylist: &str) -> bool {
    let Some(domain) = email.rsplit('@').next() else {
        return false;
    };
    let domain = domain.to_lowercase();
    denylist
        .split(',')
        .map(|d| d.trim().to_lowercase())
        .any(|d| !d.is_empty() && d == domain)
}

/// Register a user. See the module docs for the flow, the
/// no-enumeration design, and the privacy rules.
pub async fn register_user(
    pool: &DbPool,
    settings: &Settings,
    req: &RegisterRequest,
) -> Result<User, RegisterError> {
    let email = req.normalized_email();
    let tos_version = req.accept_tos_version.trim().to_string();

    // Policy checks (settings-dependent).
    let mut errors = ValidationErrors::default();
    if is_disposable(&email, &settings.disposable_email_domains) {
        errors.push(FieldError::new(
            "email",
            "disposable_email",
            "disposable email addresses are not allowed",
        ));
    }
    if tos_version != settings.tos_current_version {
        errors.push(FieldError::new(
            "accept_tos_version",
            "tos_version_mismatch",
            "must accept the current ToS version",
        ));
    }
    if !errors.is_empty() {
        return Err(RegisterError::Invalid(errors));
    }

    // Hash before any DB write (CPU work, no transaction held).
    let params = Argon2Params::from_settings(settings);
    let hash = hash_password(&req.password, &params).map_err(|err: PasswordError| {
        tracing::error!(error = ?err, "register: password hashing failed");
        RegisterError::Internal
    })?;

    // Audit the attempt outside the transaction: it must survive
    // a duplicate (409) or a rollback — it is the record that
    // someone tried.
    let email_hash = sha256_hex(email.as_bytes());
    audit_logs::insert(
        pool,
        &NewAuditLog {
            actor_user_id: None,
            actor_client: None,
            action: "register.attempt".to_string(),
            target_type: "user".to_string(),
            target_id: None,
            before: None,
            after: Some(serde_json::json!({ "email_hash": email_hash })),
            reason: None,
        },
    )
    .await
    .map_err(|err| {
        tracing::error!(error = ?err, "register: attempt audit failed");
        RegisterError::Internal
    })?;

    // One transaction: user + success audit + outbox enqueue.
    let timeout = PoolConfig::default().transaction_timeout;
    let user = tx::with_tx(pool, timeout, |conn| {
        let email = email.clone();
        let hash = hash.clone();
        let tos_version = tos_version.clone();
        let email_hash = email_hash.clone();
        Box::pin(async move {
            let user = users::create_with_tos(&mut *conn, &email, &hash, Some(&tos_version))
                .await
                .map_err(|err| match err {
                    RepoError::UniqueViolation { .. } => RegisterTxError::Duplicate,
                    other => RegisterTxError::Db(other),
                })?;
            audit_logs::insert(
                &mut *conn,
                &NewAuditLog {
                    actor_user_id: Some(user.id),
                    actor_client: None,
                    action: "register.success".to_string(),
                    target_type: "user".to_string(),
                    target_id: Some(user.id.to_string()),
                    before: None,
                    after: Some(serde_json::json!({ "email_hash": email_hash })),
                    reason: None,
                },
            )
            .await
            .map_err(RegisterTxError::Db)?;
            email_outbox::enqueue(
                &mut *conn,
                &NewOutboxMail {
                    to_hash: email_hash,
                    template: VERIFY_EMAIL_TEMPLATE.to_string(),
                    payload: serde_json::json!({ "user_id": user.id }),
                    scheduled_at: None,
                },
            )
            .await
            .map_err(RegisterTxError::Db)?;
            Ok(user)
        })
    })
    .await
    .map_err(|err| match err {
        TxError::Inner(RegisterTxError::Duplicate) => RegisterError::Duplicate,
        TxError::Inner(RegisterTxError::Db(db_err)) => {
            tracing::error!(error = ?db_err, "register: transaction failed (db)");
            RegisterError::Internal
        }
        other => {
            tracing::error!(error = ?other, "register: transaction failed");
            RegisterError::Internal
        }
    })?;
    Ok(user)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disposable_match_is_case_insensitive_and_trims() {
        let list = "mailinator.com, TempMail.com , 10minutemail.com";
        assert!(is_disposable("a@mailinator.com", list));
        assert!(is_disposable("a@MAILINATOR.COM", list));
        assert!(is_disposable("a@tempmail.com", list));
        assert!(!is_disposable("a@example.com", list));
        assert!(!is_disposable("a@mailinator.com.evil.com", list));
        assert!(!is_disposable("a@example.com", ""));
        // A subdomain is NOT the listed domain.
        assert!(!is_disposable("a@sub.mailinator.com", list));
    }

    #[test]
    fn validate_collects_every_failure() {
        let req = RegisterRequest {
            email: "not-an-email".to_string(),
            password: "short".to_string(),
            accept_tos_version: "   ".to_string(),
        };
        let err = req.validate().expect_err("all three fields fail");
        assert_eq!(err.len(), 3);
        assert_eq!(err.0[0].field, "email");
        assert_eq!(err.0[1].field, "password");
        assert_eq!(err.0[2].field, "accept_tos_version");
    }

    #[test]
    fn validate_accepts_a_good_request() {
        let req = RegisterRequest {
            email: "  Alice@Example.com ".to_string(),
            password: "correct horse battery staple".to_string(),
            accept_tos_version: "v1".to_string(),
        };
        assert!(req.validate().is_ok());
        assert_eq!(req.normalized_email(), "alice@example.com");
    }
}
