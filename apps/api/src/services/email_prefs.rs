//! Email preferences (P1-103): the self-serve toggles for the
//! mail the user can opt out of.
//!
//! The prefs live in `users.settings` jsonb, two top-level
//! boolean keys — `weekly_digest` and `marketing` — each
//! defaulting to ON (opted in); exactly `false` means opted
//! out (absent, `true`, or a non-bool means opted in — the
//! P1-101 contract for `weekly_digest`, kept at the top level
//! on purpose: namespacing the key under `email` would orphan
//! the opt-out P1-101's live tests pin, ASSUMPTION-064).
//! The merge is additive: a PUT changes only the keys it
//! names and preserves everything else in `settings` — the
//! column also carries feature flags and anything else the
//! app stores per user, so a prefs write must never replace
//! the whole object.
//!
//! What each pref gates:
//!
//! * `weekly_digest` — the weekly digest (P1-101): the
//!   sender checks it at enqueue time
//!   ([`weekly_digest_opted_out`], which delegates here).
//! * `marketing` — future marketing mail
//!   (`marketing-newsletter`, `MailClass::Marketing`): no
//!   marketing mail is sent today (no template files, the
//!   0/hr cap parks every row), so the gate lives at the
//!   drain — the last mile — where a Marketing-class row for
//!   an opted-out user is parked before render, so a future
//!   sender that forgets the check cannot mail through it.
//!
//! Transactional mail is EXEMPT by construction: no prefs
//! check runs on any other class — the drain never consults
//! prefs for transactional, verify, or reset mail, and the
//! drain test pins a fully opted-out user still receiving
//! a transactional mail. An opt-out can never silence a
//! password reset or a quota warning.

use crate::AppState;
use detag_core::{
    audit::{record, NewAuditLog},
    db::PoolConfig,
    error::FieldError,
    repos::users::{self, User},
    tx::{self, TxError},
};
use detag_validator::{Validate, ValidationErrors};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use utoipa::ToSchema;
use uuid::Uuid;

/// The two email prefs, each defaulting to opted in.
/// Serialized with the same key names the settings jsonb
/// carries, so the response is the stored shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EmailPrefs {
    /// The weekly digest (P1-101): `true` = send it.
    pub weekly_digest: bool,
    /// Future marketing mail: `true` = send it.
    pub marketing: bool,
}

impl Default for EmailPrefs {
    fn default() -> Self {
        Self {
            weekly_digest: true,
            marketing: true,
        }
    }
}

impl EmailPrefs {
    /// Read the prefs out of a `users.settings` value: each
    /// key is opted out only when it is exactly `false`.
    /// Absent, `true`, or a non-bool value means opted in —
    /// the default is ON, the flag is the OFF switch.
    pub fn from_settings(settings: &Value) -> Self {
        Self {
            weekly_digest: settings.get("weekly_digest") != Some(&Value::Bool(false)),
            marketing: settings.get("marketing") != Some(&Value::Bool(false)),
        }
    }
}

/// Whether the user opted out of the weekly digest: the
/// `weekly_digest` key in `users.settings` is exactly
/// `false`. Absent, `true`, or a non-bool value means opted
/// in — the default is ON, the flag is the OFF switch.
/// This is the single source of truth; the weekly-digest
/// service delegates here (P1-101's function kept its name
/// so its tests keep pinning the same behavior).
pub fn weekly_digest_opted_out(settings: &Value) -> bool {
    !EmailPrefs::from_settings(settings).weekly_digest
}

/// Whether the user opted out of marketing mail: the
/// `marketing` key in `users.settings` is exactly `false`.
pub fn marketing_opted_out(settings: &Value) -> bool {
    !EmailPrefs::from_settings(settings).marketing
}

/// Merge a prefs patch into a settings value: only the keys
/// the patch names change; every other key in `settings`
/// survives untouched — including keys this module does not
/// know about. A non-object `settings` (a corrupt row) is
/// treated as empty rather than crashing the write.
pub fn merge_settings(settings: &Value, patch: &UpdateEmailPreferencesRequest) -> Value {
    let mut merged = match settings.clone() {
        Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    if let Some(weekly_digest) = patch.weekly_digest {
        merged.insert("weekly_digest".to_string(), Value::Bool(weekly_digest));
    }
    if let Some(marketing) = patch.marketing {
        merged.insert("marketing".to_string(), Value::Bool(marketing));
    }
    Value::Object(merged)
}

/// The PUT body: the prefs to change. At least one must be
/// present (an empty body is 422); unknown fields are
/// rejected (a typo'd key must not silently do nothing).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateEmailPreferencesRequest {
    /// Opt the weekly digest in (`true`) or out (`false`).
    pub weekly_digest: Option<bool>,
    /// Opt marketing mail in (`true`) or out (`false`).
    pub marketing: Option<bool>,
}

impl Validate for UpdateEmailPreferencesRequest {
    /// Structural validation only: serde already enforced the
    /// bool types and `deny_unknown_fields` rejected unknown
    /// keys. The at-least-one rule runs in
    /// [`update_email_preferences`] so the 422 carries the
    /// same field-error shape as the other services.
    fn validate(&self) -> Result<(), ValidationErrors> {
        Ok(())
    }
}

/// What a prefs update can fail with.
#[derive(Debug, Error)]
pub enum EmailPrefsError {
    #[error("email preferences validation failed")]
    Validation(ValidationErrors),
    #[error("email preferences update failed")]
    Internal,
}

/// The transaction's inner error (update + audit, one tx).
#[derive(Debug)]
enum EmailPrefsTxError {
    Db(detag_core::repos::RepoError),
}

/// PUT /v1/me/email-preferences: set the caller's email
/// prefs. `actor_user_id` is the audit actor (P1-011): the
/// impersonator when the caller is impersonating, else
/// `user.id`. Returns the updated user; the handler reads the
/// prefs back from it.
pub async fn update_email_preferences(
    state: &AppState,
    user: &User,
    actor_user_id: Uuid,
    req: &UpdateEmailPreferencesRequest,
) -> Result<User, EmailPrefsError> {
    if req.weekly_digest.is_none() && req.marketing.is_none() {
        let mut errors = ValidationErrors::default();
        errors.push(FieldError::new(
            "weekly_digest",
            "required",
            "at least one of weekly_digest or marketing must be provided",
        ));
        return Err(EmailPrefsError::Validation(errors));
    }

    let current = EmailPrefs::from_settings(&user.settings);
    let merged = merge_settings(&user.settings, req);
    let updated_prefs = EmailPrefs::from_settings(&merged);

    // No-op: nothing changes — return the user without a
    // write or an audit (the PATCH /v1/me + consent
    // precedent).
    if updated_prefs == current {
        return Ok(user.clone());
    }

    // One transaction: the settings update + the audit. The
    // audit's before/after carry the two booleans (plain
    // booleans — a pref is not PII).
    let before = serde_json::json!({
        "weekly_digest": current.weekly_digest,
        "marketing": current.marketing,
    });
    let after = serde_json::json!({
        "weekly_digest": updated_prefs.weekly_digest,
        "marketing": updated_prefs.marketing,
    });
    let timeout = PoolConfig::default().transaction_timeout;
    let updated = tx::with_tx(&state.pool, timeout, |conn| {
        let merged = merged.clone();
        let before = before.clone();
        let after = after.clone();
        let user = user.clone();
        Box::pin(async move {
            let updated = users::update_settings(&mut *conn, user.id, &merged)
                .await
                .map_err(EmailPrefsTxError::Db)?;
            record(
                &mut *conn,
                &NewAuditLog {
                    actor_user_id: Some(actor_user_id),
                    actor_client: None,
                    action: "user.email_prefs_updated".to_string(),
                    target_type: "user".to_string(),
                    target_id: Some(user.id.to_string()),
                    before: Some(before),
                    after: Some(after),
                    reason: None,
                },
            )
            .await
            .map_err(EmailPrefsTxError::Db)?;
            Ok(updated)
        })
    })
    .await
    .map_err(|err| match err {
        TxError::Inner(EmailPrefsTxError::Db(db_err)) => {
            tracing::error!(error = ?db_err, "email_prefs: update transaction failed (db)");
            EmailPrefsError::Internal
        }
        other => {
            tracing::error!(error = ?other, "email_prefs: update transaction failed");
            EmailPrefsError::Internal
        }
    })?;
    Ok(updated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn prefs_default_to_opted_in() {
        assert_eq!(
            EmailPrefs::from_settings(&json!({})),
            EmailPrefs {
                weekly_digest: true,
                marketing: true
            }
        );
        assert_eq!(
            EmailPrefs::from_settings(&json!({"weekly_digest": true, "marketing": true})),
            EmailPrefs {
                weekly_digest: true,
                marketing: true
            }
        );
        // A non-bool value is not an opt-out — the key is
        // mistyped, not a choice; the default (opted in) wins.
        assert_eq!(
            EmailPrefs::from_settings(&json!({"weekly_digest": "no", "marketing": 0})),
            EmailPrefs {
                weekly_digest: true,
                marketing: true
            }
        );
        assert_eq!(
            EmailPrefs::from_settings(&Value::Null),
            EmailPrefs {
                weekly_digest: true,
                marketing: true
            }
        );
    }

    #[test]
    fn prefs_opt_out_only_when_the_key_is_exactly_false() {
        assert!(weekly_digest_opted_out(&json!({"weekly_digest": false})));
        assert!(!weekly_digest_opted_out(&json!({"weekly_digest": true})));
        assert!(!weekly_digest_opted_out(&json!({})));
        assert!(marketing_opted_out(&json!({"marketing": false})));
        assert!(!marketing_opted_out(&json!({"marketing": true})));
        assert!(!marketing_opted_out(&json!({})));
        // The two prefs are independent: opting out of one
        // leaves the other on.
        let prefs = EmailPrefs::from_settings(&json!({"weekly_digest": false}));
        assert!(!prefs.weekly_digest);
        assert!(prefs.marketing);
        let prefs = EmailPrefs::from_settings(&json!({"marketing": false}));
        assert!(prefs.weekly_digest);
        assert!(!prefs.marketing);
    }

    #[test]
    fn merge_changes_only_the_named_keys() {
        let settings = json!({"weekly_digest": true, "feature_x": "keep-me", "n": 3});
        let merged = merge_settings(
            &settings,
            &UpdateEmailPreferencesRequest {
                weekly_digest: Some(false),
                marketing: None,
            },
        );
        assert_eq!(
            merged,
            json!({"weekly_digest": false, "feature_x": "keep-me", "n": 3})
        );
        // A patch that names nothing leaves the object alone.
        let merged = merge_settings(
            &settings,
            &UpdateEmailPreferencesRequest {
                weekly_digest: None,
                marketing: None,
            },
        );
        assert_eq!(merged, settings);
    }
}
