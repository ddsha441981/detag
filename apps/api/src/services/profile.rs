//! Profile service (P1-005): GET /v1/me + PATCH /v1/me.
//!
//! GET returns the profile (id, email, display name,
//! verification state, role, status) + the current plan +
//! today's quota snapshot (from the entitlements service —
//! the plan's daily limits against today's durable usage row).
//!
//! PATCH sets the display name and/or records an email-change
//! INTENT (`pending_email`): the address is validated (shape,
//! disposable denylist — the same rule as registration) and
//! checked for duplicates, but it is NOT verified or swapped
//! here — P1-006 mints the verification token, notifies the
//! old address, and swaps on verify. Passing `email: null`
//! clears a pending intent; passing no fields is a no-op
//! (200 with the current profile, no audit, no update).
//!
//! Concurrency: two concurrent PATCHes are last-write-wins
//! (the update is one row write + one audit row in a
//! transaction, but the base values are read before the tx —
//! a concurrent PATCH can clobber the other's field). That is
//! acceptable for a profile endpoint (the same shape as the
//! register/verify flows, which do not lock the user row
//! either); a compare-and-swap would be over-engineering for
//! a display name.

use super::verification::{clear_email_change_tokens, issue_email_change};
use super::{entitlements, register::is_disposable};
use crate::AppState;
use chrono::{DateTime, Utc};
use detag_core::{
    checksum::sha256_hex,
    db::PoolConfig,
    error::FieldError,
    repos::{
        audit_logs::{self, NewAuditLog},
        users::{self, User},
        RepoError,
    },
    tx::{self, TxError},
};
use detag_validator::{validate_display_name, validate_email, Validate, ValidationErrors};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use utoipa::ToSchema;
use uuid::Uuid;

/// The profile response: the user + the current plan +
/// today's quota snapshot. The password hash is never here
/// (P1-003); the pending email IS shown (the client needs to
/// know a change is pending — it is the user's own address).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ProfileResponse {
    /// The user id.
    pub id: Uuid,
    /// The current (verified or not) email address.
    pub email: String,
    /// The display name, when set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Whether the email is verified.
    pub email_verified: bool,
    /// When the email was verified (None until then).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email_verified_at: Option<DateTime<Utc>>,
    /// A pending email change (P1-005 records it; P1-006
    /// verifies + swaps it). None when no change is pending.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_email: Option<String>,
    /// The role (`user` / `support` / `admin`).
    pub role: String,
    /// The status (`active` / `suspended` / `deletion_pending`).
    pub status: String,
    /// When the deletion was requested (set only during the
    /// 30-day deletion grace, P1-007 — the account is purged
    /// 30 days after this timestamp unless a login cancels it).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<DateTime<Utc>>,
    /// When the account was created.
    pub created_at: DateTime<Utc>,
    /// The current plan (the live subscription's plan, else
    /// Free).
    pub plan: PlanResponse,
    /// Today's quota snapshot (daily limits vs today's usage).
    pub quota: QuotaResponse,
}

/// The plan section of the profile.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PlanResponse {
    /// The plan code (`free` / `pro` / `business`).
    pub code: String,
    /// The price in the smallest currency unit (paise for INR).
    pub amount: i32,
    /// The currency (`INR`).
    pub currency: String,
}

/// One quota bucket: the daily limit, today's usage, and
/// what is left.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct QuotaBucketResponse {
    pub limit: i64,
    pub used: i64,
    pub remaining: i64,
}

/// Today's quota snapshot: the two daily buckets.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct QuotaResponse {
    pub daily_images: QuotaBucketResponse,
    pub daily_video_seconds: QuotaBucketResponse,
}

/// Deserialize a double `Option` so JSON `null` means "clear"
/// (`Some(None)`) while a missing field means "leave unchanged"
/// (`None`, via `#[serde(default)]`). Plain serde derive
/// collapses the two — `Option<Option<T>>` deserializes `null`
/// as the outer `None` (proven by the
/// `patch_request_distinguishes_null_from_missing` unit test
/// before this function existed) — so the explicit
/// `deserialize_with` is what keeps them apart.
fn deserialize_double_option<'de, D>(deserializer: D) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Some(Option::deserialize(deserializer)?))
}

/// The PATCH body: each field is `Option<Option<..>>` —
/// absent = leave unchanged, `null` = clear, a value = set.
/// (`#[serde(default)]` supplies the absent case; the custom
/// deserializer supplies the null case — see above.)
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct PatchProfileRequest {
    /// The display name to set (`null` clears it).
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub display_name: Option<Option<String>>,
    /// The new email address — records an email-change intent
    /// (`null` clears a pending intent). P1-006 verifies it.
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub email: Option<Option<String>>,
}

impl Validate for PatchProfileRequest {
    fn validate(&self) -> Result<(), ValidationErrors> {
        let mut errors = ValidationErrors::default();
        if let Some(Some(name)) = &self.display_name {
            if let Err(err) = validate_display_name("display_name", name) {
                errors.push(err);
            }
        }
        if let Some(Some(email)) = &self.email {
            let normalized = email.trim().to_lowercase();
            if let Err(err) = validate_email("email", &normalized) {
                errors.push(err);
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

/// What a profile read/update can fail with. `Duplicate` is
/// the email-change intent colliding with another account's
/// address (409 `PF-2011`, the same answer as registration).
#[derive(Debug, Error)]
pub enum ProfileError {
    #[error("profile validation failed")]
    Validation(ValidationErrors),
    #[error("email already registered")]
    Duplicate,
    #[error("profile failed")]
    Internal,
}

/// The transaction's inner error (update + audit, one tx).
#[derive(Debug)]
enum ProfileTxError {
    Db(RepoError),
}

/// GET /v1/me: the profile + the current plan + today's
/// quota snapshot. `user` is the extractor's verified user
/// (fresh enough for a read — the plan/quota reads are the
/// live part).
pub async fn get_profile(state: &AppState, user: &User) -> Result<ProfileResponse, ProfileError> {
    let plan = entitlements::current_plan_row(&state.pool, user.id)
        .await
        .map_err(|err| {
            tracing::error!(error = ?err, "profile: plan read failed");
            ProfileError::Internal
        })?;
    let quota = entitlements::quota_snapshot_for_plan(&state.pool, user.id, &plan)
        .await
        .map_err(|err| {
            tracing::error!(error = ?err, "profile: quota read failed");
            ProfileError::Internal
        })?;
    Ok(profile_response(user, &plan, &quota))
}

/// PATCH /v1/me: set the display name and/or record an
/// email-change intent. `user` is the extractor's verified
/// user (the base values for the patch).
pub async fn patch_profile(
    state: &AppState,
    user: &User,
    req: &PatchProfileRequest,
) -> Result<ProfileResponse, ProfileError> {
    // Display name: validate (again — the handler's
    // ValidatedJson is the gate; this call is the transform
    // that yields the trimmed value to store).
    let new_display_name = match &req.display_name {
        None => user.display_name.clone(),
        Some(None) => None,
        Some(Some(name)) => Some(
            validate_display_name("display_name", name)
                .map_err(|err| ProfileError::Validation(ValidationErrors(vec![err])))?,
        ),
    };
    // Email-change intent: normalize, then the policy checks
    // (unchanged, disposable, duplicate). The duplicate check
    // is a courtesy (fail fast) — the swap in P1-006 enforces
    // uniqueness via the citext unique constraint on `email`.
    let new_pending_email = match &req.email {
        None => user.pending_email.clone(),
        Some(None) => None,
        Some(Some(email)) => {
            let email = email.trim().to_lowercase();
            if email == user.email.to_lowercase() {
                return Err(ProfileError::Validation(ValidationErrors(vec![
                    FieldError::new(
                        "email",
                        "email_unchanged",
                        "is the same as the current email address",
                    ),
                ])));
            }
            if is_disposable(&email, &state.settings.disposable_email_domains) {
                return Err(ProfileError::Validation(ValidationErrors(vec![
                    FieldError::new(
                        "email",
                        "disposable_email",
                        "disposable email addresses are not allowed",
                    ),
                ])));
            }
            if let Some(other) = users::by_email(&state.pool, &email).await.map_err(|err| {
                tracing::error!(error = ?err, "profile: duplicate check failed");
                ProfileError::Internal
            })? {
                if other.id != user.id {
                    return Err(ProfileError::Duplicate);
                }
            }
            Some(email)
        }
    };

    // No-op: nothing changed — return the current profile
    // without a write or an audit.
    if new_display_name == user.display_name && new_pending_email == user.pending_email {
        return get_profile(state, user).await;
    }

    // One transaction: the profile update + the audit. The
    // audit's `after` carries only what changed — the display
    // name plainly (it is public profile data) and the pending
    // email as a sha256 (the P1-001 rule: no raw email in
    // audits or outbox rows).
    let mut after = serde_json::Map::new();
    if new_display_name != user.display_name {
        after.insert(
            "display_name".to_string(),
            serde_json::json!(new_display_name),
        );
    }
    if new_pending_email != user.pending_email {
        after.insert(
            "pending_email_hash".to_string(),
            serde_json::json!(new_pending_email.as_ref().map(|e| sha256_hex(e.as_bytes()))),
        );
    }
    let timeout = PoolConfig::default().transaction_timeout;
    let updated = tx::with_tx(&state.pool, timeout, |conn| {
        let new_display_name = new_display_name.clone();
        let new_pending_email = new_pending_email.clone();
        let after = serde_json::Value::Object(after.clone());
        let user = user.clone();
        Box::pin(async move {
            let updated = users::update_profile(
                &mut *conn,
                user.id,
                new_display_name,
                new_pending_email.clone(),
            )
            .await
            .map_err(ProfileTxError::Db)?;
            // Email-change trigger (P1-006), in the same tx as
            // the profile update + audit: a NEW pending address
            // mints the change token and queues the verification
            // mail (to the new address) + the takeover alert
            // (to the old address); clearing an intent kills
            // the live change token. The raw token is dropped
            // here on purpose — it lives only in the outbox
            // payload (where the mail drain needs it) and in
            // the email itself, never in a log or a response.
            if new_pending_email != user.pending_email {
                match &new_pending_email {
                    Some(addr) => {
                        let _raw = issue_email_change(&mut *conn, &user, addr)
                            .await
                            .map_err(ProfileTxError::Db)?;
                    }
                    None => {
                        clear_email_change_tokens(&mut *conn, user.id)
                            .await
                            .map_err(ProfileTxError::Db)?;
                    }
                }
            }
            audit_logs::insert(
                &mut *conn,
                &NewAuditLog {
                    actor_user_id: Some(user.id),
                    actor_client: None,
                    action: "profile.update".to_string(),
                    target_type: "user".to_string(),
                    target_id: Some(user.id.to_string()),
                    before: None,
                    after: Some(after),
                    reason: None,
                },
            )
            .await
            .map_err(ProfileTxError::Db)?;
            Ok(updated)
        })
    })
    .await
    .map_err(|err| match err {
        TxError::Inner(ProfileTxError::Db(db_err)) => {
            tracing::error!(error = ?db_err, "profile: update transaction failed (db)");
            ProfileError::Internal
        }
        other => {
            tracing::error!(error = ?other, "profile: update transaction failed");
            ProfileError::Internal
        }
    })?;

    let plan = entitlements::current_plan_row(&state.pool, updated.id)
        .await
        .map_err(|err| {
            tracing::error!(error = ?err, "profile: plan read failed");
            ProfileError::Internal
        })?;
    let quota = entitlements::quota_snapshot_for_plan(&state.pool, updated.id, &plan)
        .await
        .map_err(|err| {
            tracing::error!(error = ?err, "profile: quota read failed");
            ProfileError::Internal
        })?;
    Ok(profile_response(&updated, &plan, &quota))
}

fn profile_response(
    user: &User,
    plan: &detag_core::repos::plans::Plan,
    quota: &entitlements::QuotaSnapshot,
) -> ProfileResponse {
    ProfileResponse {
        id: user.id,
        email: user.email.clone(),
        display_name: user.display_name.clone(),
        email_verified: user.email_verified_at.is_some(),
        email_verified_at: user.email_verified_at,
        pending_email: user.pending_email.clone(),
        role: user.role.clone(),
        status: user.status.clone(),
        deleted_at: user.deleted_at,
        created_at: user.created_at,
        plan: PlanResponse {
            code: plan.code.clone(),
            amount: plan.amount,
            currency: plan.currency.clone(),
        },
        quota: QuotaResponse {
            daily_images: QuotaBucketResponse {
                limit: quota.daily_images.limit,
                used: quota.daily_images.used,
                remaining: quota.daily_images.remaining,
            },
            daily_video_seconds: QuotaBucketResponse {
                limit: quota.daily_video_seconds.limit,
                used: quota.daily_video_seconds.used,
                remaining: quota.daily_video_seconds.remaining,
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patch_request_validates_display_name_shape() {
        let req = PatchProfileRequest {
            display_name: Some(Some("   ".to_string())),
            email: None,
        };
        let err = req.validate().expect_err("blank name must fail");
        assert_eq!(err.0[0].field, "display_name");
        assert_eq!(err.0[0].code, "required");
    }

    #[test]
    fn patch_request_validates_email_shape() {
        let req = PatchProfileRequest {
            display_name: None,
            email: Some(Some("not-an-email".to_string())),
        };
        let err = req.validate().expect_err("bad email must fail");
        assert_eq!(err.0[0].field, "email");
        assert_eq!(err.0[0].code, "invalid_email");
    }

    #[test]
    fn patch_request_accepts_nulls_as_clear() {
        let req = PatchProfileRequest {
            display_name: Some(None),
            email: Some(None),
        };
        req.validate().expect("nulls are valid (clear)");
    }

    #[test]
    fn patch_request_distinguishes_null_from_missing() {
        // `null` means "clear" (Some(None)); a missing field
        // means "leave unchanged" (None). Plain serde derive
        // collapses the two — the custom deserializer below is
        // what keeps them apart.
        let req: PatchProfileRequest =
            serde_json::from_value(serde_json::json!({"display_name": null, "email": null}))
                .expect("nulls parse");
        assert_eq!(req.display_name, Some(None));
        assert_eq!(req.email, Some(None));
        let req: PatchProfileRequest =
            serde_json::from_value(serde_json::json!({})).expect("empty body parses");
        assert_eq!(req.display_name, None);
        assert_eq!(req.email, None);
        let req: PatchProfileRequest =
            serde_json::from_value(serde_json::json!({"display_name": "Deen"}))
                .expect("value parses");
        assert_eq!(req.display_name, Some(Some("Deen".to_string())));
    }

    #[test]
    fn patch_request_accepts_empty_body() {
        let req: PatchProfileRequest =
            serde_json::from_value(serde_json::json!({})).expect("empty body parses");
        assert!(req.display_name.is_none() && req.email.is_none());
        req.validate().expect("empty patch is valid (no-op)");
    }
}
