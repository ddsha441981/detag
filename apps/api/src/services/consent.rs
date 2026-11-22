//! Consent service (P1-095): the re-accept banner + endpoint.
//!
//! The consent versions a user accepted live on the user row
//! (`tos_version` + `aup_version`, migration
//! 20261121182606); the current versions live in config
//! (`PF_TOS_CURRENT_VERSION` / `PF_AUP_CURRENT_VERSION`).
//! Bumping either current version flips every account whose
//! accepted version is older to "must re-accept": GET /v1/me
//! reports it in the `consent` section (`up_to_date: false` —
//! the banner), and POST /v1/me/consent is the only way to
//! clear it (accept the current versions, audited).
//!
//! The bump does NOT gate any other endpoint: no login or job
//! block, no forced logout. That is a deliberate scope
//! decision (ASSUMPTION-056) — the spec names the banner, and
//! gating is a future owner decision. The banner is the
//! enforcement surface in this ticket.
//!
//! Re-acceptance is one transaction: the row update +
//! `user.consent_accepted` audit row (actor the caller,
//! target the user, before/after the two versions). An
//! already-current re-accept is a no-op 200 — no write, no
//! audit — the PATCH /v1/me precedent: a request that changes
//! nothing writes nothing.

use crate::AppState;
use detag_core::{
    audit::{record, NewAuditLog},
    config::Settings,
    db::PoolConfig,
    error::FieldError,
    repos::users::{self, User},
    tx::{self, TxError},
};
use detag_validator::{Validate, ValidationErrors};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use utoipa::ToSchema;
use uuid::Uuid;

/// The consent section of GET /v1/me (and of the re-accept
/// response): the versions the user accepted, the versions
/// current in config, and whether the user is up to date.
/// `None` accepted versions mean "never accepted" — stale by
/// definition, so a pre-P1-095 account shows the banner until
/// it re-accepts once.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ConsentStatus {
    /// The ToS version the user accepted (`None` = never).
    pub tos_version: Option<String>,
    /// The AUP version the user accepted (`None` = never).
    pub aup_version: Option<String>,
    /// The current ToS version (config).
    pub tos_current_version: String,
    /// The current AUP version (config).
    pub aup_current_version: String,
    /// False when either accepted version is older than (or
    /// missing against) its current version — the banner.
    pub up_to_date: bool,
}

impl ConsentStatus {
    /// The user's consent state against the current config.
    pub fn from_user(user: &User, settings: &Settings) -> Self {
        let up_to_date = user.tos_version.as_deref() == Some(settings.tos_current_version.as_str())
            && user.aup_version.as_deref() == Some(settings.aup_current_version.as_str());
        Self {
            tos_version: user.tos_version.clone(),
            aup_version: user.aup_version.clone(),
            tos_current_version: settings.tos_current_version.clone(),
            aup_current_version: settings.aup_current_version.clone(),
            up_to_date,
        }
    }
}

/// The re-accept body: both versions the user accepts now.
/// Each must equal its current version (the same rule as
/// registration) — accepting a stale version cannot clear the
/// banner, so the handler rejects it instead of storing it.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct AcceptConsentRequest {
    /// The ToS version the user accepts; must equal the
    /// current version (`PF_TOS_CURRENT_VERSION`).
    pub accept_tos_version: String,
    /// The AUP version the user accepts; must equal the
    /// current version (`PF_AUP_CURRENT_VERSION`).
    pub accept_aup_version: String,
}

impl Validate for AcceptConsentRequest {
    /// Structural validation only (no settings): both versions
    /// present. The settings-dependent checks (each equals its
    /// current version) run in [`accept_consent`] and produce
    /// the same 422 shape.
    fn validate(&self) -> Result<(), ValidationErrors> {
        let mut errors = ValidationErrors::default();
        if self.accept_tos_version.trim().is_empty() {
            errors.push(FieldError::new(
                "accept_tos_version",
                "required",
                "is required",
            ));
        }
        if self.accept_aup_version.trim().is_empty() {
            errors.push(FieldError::new(
                "accept_aup_version",
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

/// What a re-accept can fail with.
#[derive(Debug, Error)]
pub enum ConsentError {
    #[error("consent validation failed")]
    Validation(ValidationErrors),
    #[error("consent failed")]
    Internal,
}

/// The transaction's inner error (update + audit, one tx).
#[derive(Debug)]
enum ConsentTxError {
    Db(detag_core::repos::RepoError),
}

/// POST /v1/me/consent: accept the current ToS + AUP versions.
/// `actor_user_id` is the audit actor (P1-011): the
/// impersonator when the caller is impersonating, else
/// `user.id`. Returns the updated user; the handler re-reads
/// the profile from it.
pub async fn accept_consent(
    state: &AppState,
    user: &User,
    actor_user_id: Uuid,
    req: &AcceptConsentRequest,
) -> Result<User, ConsentError> {
    let settings = &state.settings;
    let tos_version = req.accept_tos_version.trim().to_string();
    let aup_version = req.accept_aup_version.trim().to_string();

    // Policy checks (settings-dependent): each accepted version
    // must equal its current version — a stale acceptance
    // cannot clear the banner.
    let mut errors = ValidationErrors::default();
    if tos_version != settings.tos_current_version {
        errors.push(FieldError::new(
            "accept_tos_version",
            "tos_version_mismatch",
            "must accept the current ToS version",
        ));
    }
    if aup_version != settings.aup_current_version {
        errors.push(FieldError::new(
            "accept_aup_version",
            "aup_version_mismatch",
            "must accept the current AUP version",
        ));
    }
    if !errors.is_empty() {
        return Err(ConsentError::Validation(errors));
    }

    // No-op: already current — return the user without a write
    // or an audit (the PATCH /v1/me precedent).
    if user.tos_version.as_deref() == Some(tos_version.as_str())
        && user.aup_version.as_deref() == Some(aup_version.as_str())
    {
        return Ok(user.clone());
    }

    // One transaction: the row update + the audit. The audit's
    // before/after carry the two versions (plain strings — a
    // version is not PII).
    let before = serde_json::json!({
        "tos_version": user.tos_version,
        "aup_version": user.aup_version,
    });
    let after = serde_json::json!({
        "tos_version": tos_version,
        "aup_version": aup_version,
    });
    let timeout = PoolConfig::default().transaction_timeout;
    let updated = tx::with_tx(&state.pool, timeout, |conn| {
        let tos_version = tos_version.clone();
        let aup_version = aup_version.clone();
        let before = before.clone();
        let after = after.clone();
        let user = user.clone();
        Box::pin(async move {
            let updated = users::update_consent(&mut *conn, user.id, &tos_version, &aup_version)
                .await
                .map_err(ConsentTxError::Db)?;
            record(
                &mut *conn,
                &NewAuditLog {
                    actor_user_id: Some(actor_user_id),
                    actor_client: None,
                    action: "user.consent_accepted".to_string(),
                    target_type: "user".to_string(),
                    target_id: Some(user.id.to_string()),
                    before: Some(before),
                    after: Some(after),
                    reason: None,
                },
            )
            .await
            .map_err(ConsentTxError::Db)?;
            Ok(updated)
        })
    })
    .await
    .map_err(|err| match err {
        TxError::Inner(ConsentTxError::Db(db_err)) => {
            tracing::error!(error = ?db_err, "consent: update transaction failed (db)");
            ConsentError::Internal
        }
        other => {
            tracing::error!(error = ?other, "consent: update transaction failed");
            ConsentError::Internal
        }
    })?;
    Ok(updated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use detag_core::repos::users::User;
    use serde_json::json;

    fn user_with(tos: Option<&str>, aup: Option<&str>) -> User {
        User {
            id: Uuid::now_v7(),
            email: "consent@example.com".to_string(),
            password_hash: String::new(),
            email_verified_at: None,
            role: "user".to_string(),
            status: "active".to_string(),
            tos_version: tos.map(str::to_string),
            aup_version: aup.map(str::to_string),
            deleted_at: None,
            revoked_before: None,
            display_name: None,
            pending_email: None,
            settings: json!({}),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn consent_status_is_up_to_date_only_when_both_match() {
        let settings = Settings::default();
        assert_eq!(settings.tos_current_version, "v1");
        assert_eq!(settings.aup_current_version, "v1");
        assert!(ConsentStatus::from_user(&user_with(Some("v1"), Some("v1")), &settings).up_to_date);
        assert!(
            !ConsentStatus::from_user(&user_with(Some("v1"), Some("v2")), &settings).up_to_date
        );
        assert!(
            !ConsentStatus::from_user(&user_with(Some("v2"), Some("v1")), &settings).up_to_date
        );
        assert!(!ConsentStatus::from_user(&user_with(None, Some("v1")), &settings).up_to_date);
        assert!(!ConsentStatus::from_user(&user_with(Some("v1"), None), &settings).up_to_date);
        assert!(!ConsentStatus::from_user(&user_with(None, None), &settings).up_to_date);
    }

    #[test]
    fn consent_status_carries_both_versions() {
        let settings = Settings::default();
        let status = ConsentStatus::from_user(&user_with(Some("v1"), Some("v1")), &settings);
        assert_eq!(status.tos_version.as_deref(), Some("v1"));
        assert_eq!(status.aup_version.as_deref(), Some("v1"));
        assert_eq!(status.tos_current_version, "v1");
        assert_eq!(status.aup_current_version, "v1");
    }

    #[test]
    fn accept_request_requires_both_versions() {
        let req = AcceptConsentRequest {
            accept_tos_version: "   ".to_string(),
            accept_aup_version: "".to_string(),
        };
        let err = req.validate().expect_err("both blank must fail");
        assert_eq!(err.len(), 2);
        assert_eq!(err.0[0].field, "accept_tos_version");
        assert_eq!(err.0[1].field, "accept_aup_version");
        let req = AcceptConsentRequest {
            accept_tos_version: "v1".to_string(),
            accept_aup_version: "v1".to_string(),
        };
        assert!(req.validate().is_ok());
    }
}
