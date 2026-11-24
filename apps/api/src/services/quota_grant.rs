//! Manual quota grant (P1-107, FR-C-02): `POST
//! /v1/admin/users/{id}/quota-grant` — an admin grants a user a
//! temporary bump to their daily quota limits, with a reason,
//! audited as `admin.users.quota_granted`.
//!
//! The grant is a row in `quota_grants` (migration
//! 20261124080000): `images` / `video_seconds` (at least one,
//! each positive when present), `expires_at` (now + `ttl_hours`
//! on the app clock), the `reason`, and who granted it. The
//! grant and its audit row commit in ONE transaction (the
//! P1-008 atomicity rule — the row and the grant commit
//! together or not at all), and the handler marks the
//! response `AuditHandled` so the P1-046 middleware stands
//! down and the row exists exactly once.
//!
//! Honoring is the snapshot's job, not this service's: the
//! grant adds to the plan's daily limits in
//! [`entitlements::quota_snapshot_for_plan`] (the SUM of the
//! user's unexpired grants), so the pre-check, the profile,
//! and the quota warnings all see the same bumped limits,
//! and an expired grant stops counting the moment its
//! `expires_at` passes. This service only writes the grant.
//!
//! A grant is admin-only ([`AdminContext`] — support is
//! read-only, so support gets 403 `PF-1002` from the
//! extractor, an API key 403, no credential 401 `PF-1001`).
//! A user who does not exist is 404 `PF-2010` (the P1-081
//! precedent). Any status may be granted: the grant is a
//! limit bump, and a suspended account still cannot create
//! jobs (the login + quota gates refuse it upstream), so no
//! status gate is needed here (ASSUMPTION-068). The handler
//! answers 200 with the created grant (ASSUMPTION-068 — an
//! admin action on a user, not a 201).

use crate::AppState;
use chrono::{DateTime, Duration, Utc};
use detag_core::{
    audit::{record, NewAuditLog},
    db::PoolConfig,
    error::FieldError,
    repos::{
        quota_grants::{self, NewQuotaGrant, QuotaGrant},
        users::{self, User},
        RepoError,
    },
    tx::{self, TxError},
};
use detag_validator::{validate_reason, Validate, ValidationErrors};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use utoipa::ToSchema;
use uuid::Uuid;

/// The longest a grant may live, in hours: one week. A bump
/// longer than a week is a plan change, not a bump — the plan
/// editor (P1-056) owns that. A grant of 0 hours would expire
/// before it is read, so the floor is 1.
pub const MAX_TTL_HOURS: i64 = 168;

/// The audit action for a grant (the matrix already maps the
/// route to it — `Capability::audit_action_for`).
pub const QUOTA_GRANTED_AUDIT_ACTION: &str = "admin.users.quota_granted";

/// What a grant can fail with.
#[derive(Debug, Error)]
pub enum QuotaGrantError {
    #[error("quota grant validation failed")]
    Validation(ValidationErrors),
    #[error("user not found")]
    NotFound,
    #[error("quota grant failed")]
    Internal,
}

/// The transaction closure's error: small on purpose — it
/// must not recurse into [`QuotaGrantError`].
#[derive(Debug)]
enum QuotaGrantTxError {
    Db(RepoError),
}

fn map_tx_error(context: &'static str) -> impl Fn(TxError<QuotaGrantTxError>) -> QuotaGrantError {
    move |err| match err {
        TxError::Inner(QuotaGrantTxError::Db(db_err)) => {
            tracing::error!(error = ?db_err, "{context}: transaction failed (db)");
            QuotaGrantError::Internal
        }
        other => {
            tracing::error!(error = ?other, "{context}: transaction failed");
            QuotaGrantError::Internal
        }
    }
}

/// The grant request: at least one amount, a TTL, and the
/// reason the audit carries verbatim.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct QuotaGrantRequest {
    /// Extra daily images (optional — at least one of the
    /// two amounts must be present).
    pub images: Option<i32>,
    /// Extra daily video-seconds (optional — at least one of
    /// the two amounts must be present).
    pub video_seconds: Option<i32>,
    /// How long the grant lives, in hours (1–168).
    pub ttl_hours: i64,
    /// Why the grant is made (1–500 characters after
    /// trimming; the audit carries it verbatim).
    pub reason: String,
}

impl Validate for QuotaGrantRequest {
    fn validate(&self) -> Result<(), ValidationErrors> {
        let mut errors = ValidationErrors::default();
        if let Some(images) = self.images {
            if images < 1 {
                errors.push(FieldError::new(
                    "images",
                    "invalid_images",
                    "images must be at least 1 when present",
                ));
            }
        }
        if let Some(video_seconds) = self.video_seconds {
            if video_seconds < 1 {
                errors.push(FieldError::new(
                    "video_seconds",
                    "invalid_video_seconds",
                    "video_seconds must be at least 1 when present",
                ));
            }
        }
        if self.images.is_none() && self.video_seconds.is_none() {
            errors.push(FieldError::new(
                "images",
                "missing_amount",
                "at least one of images, video_seconds is required",
            ));
        }
        if self.ttl_hours < 1 || self.ttl_hours > MAX_TTL_HOURS {
            errors.push(FieldError::new(
                "ttl_hours",
                "invalid_ttl_hours",
                "ttl_hours must be between 1 and 168",
            ));
        }
        if let Err(err) = validate_reason("reason", &self.reason) {
            errors.push(err);
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

/// The grant, as the API returns it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct QuotaGrantResponse {
    /// The grant id.
    #[schema(value_type = String, example = "123e4567-e89b-42d3-a456-426614174109")]
    pub id: Uuid,
    /// Whose quota was bumped.
    #[schema(value_type = String, example = "123e4567-e89b-42d3-a456-426614174109")]
    pub user_id: Uuid,
    /// The extra daily images (`None` = this grant adds none).
    pub images: Option<i32>,
    /// The extra daily video-seconds (`None` = this grant adds none).
    pub video_seconds: Option<i32>,
    /// When the grant stops counting.
    pub expires_at: DateTime<Utc>,
    /// Why the grant was made.
    pub reason: String,
    /// Who granted it (the admin's id).
    #[schema(value_type = String, example = "123e4567-e89b-42d3-a456-426614174109")]
    pub granted_by: Uuid,
    /// When the grant was created.
    pub created_at: DateTime<Utc>,
}

impl From<QuotaGrant> for QuotaGrantResponse {
    fn from(grant: QuotaGrant) -> Self {
        QuotaGrantResponse {
            id: grant.id,
            user_id: grant.user_id,
            images: grant.images,
            video_seconds: grant.video_seconds,
            expires_at: grant.expires_at,
            reason: grant.reason,
            granted_by: grant.granted_by,
            created_at: grant.created_at,
        }
    }
}

/// Grant a user a temporary quota bump (P1-107). One
/// transaction: the `quota_grants` row + the
/// `admin.users.quota_granted` audit row. The grant expires
/// at `now + ttl_hours` on the app clock, so a frozen clock
/// moves expiry too.
pub async fn grant_quota(
    state: &AppState,
    admin: &User,
    target_id: Uuid,
    req: &QuotaGrantRequest,
) -> Result<QuotaGrant, QuotaGrantError> {
    req.validate().map_err(QuotaGrantError::Validation)?;
    let target = users::by_id(&state.pool, target_id).await.map_err(|err| {
        tracing::error!(error = ?err, "quota grant: user lookup failed");
        QuotaGrantError::Internal
    })?;
    let Some(target) = target else {
        return Err(QuotaGrantError::NotFound);
    };
    let now = state.clock.now();
    let expires_at = now + Duration::hours(req.ttl_hours);
    let timeout = PoolConfig::default().transaction_timeout;
    let grant = tx::with_tx(&state.pool, timeout, |conn| {
        let admin = admin.clone();
        let target = target.clone();
        let req = req.clone();
        Box::pin(async move {
            let grant = quota_grants::insert(
                &mut *conn,
                &NewQuotaGrant {
                    user_id: target.id,
                    images: req.images,
                    video_seconds: req.video_seconds,
                    expires_at,
                    reason: req.reason.clone(),
                    granted_by: admin.id,
                },
            )
            .await
            .map_err(QuotaGrantTxError::Db)?;
            record(
                &mut *conn,
                &NewAuditLog {
                    actor_user_id: Some(admin.id),
                    actor_client: None,
                    action: QUOTA_GRANTED_AUDIT_ACTION.to_string(),
                    target_type: "user".to_string(),
                    target_id: Some(target.id.to_string()),
                    before: None,
                    after: Some(serde_json::json!({
                        "images": grant.images,
                        "video_seconds": grant.video_seconds,
                        "expires_at": grant.expires_at,
                        "grant_id": grant.id,
                    })),
                    reason: Some(req.reason.clone()),
                },
            )
            .await
            .map_err(QuotaGrantTxError::Db)?;
            Ok(grant)
        })
    })
    .await
    .map_err(map_tx_error("quota grant"))?;
    Ok(grant)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid() -> QuotaGrantRequest {
        QuotaGrantRequest {
            images: Some(5),
            video_seconds: None,
            ttl_hours: 24,
            reason: "support bump for a failed upload".to_string(),
        }
    }

    #[test]
    fn valid_request_passes() {
        assert!(valid().validate().is_ok());
        let mut video_only = valid();
        video_only.images = None;
        video_only.video_seconds = Some(300);
        assert!(video_only.validate().is_ok());
        let mut both = valid();
        both.video_seconds = Some(300);
        assert!(both.validate().is_ok());
    }

    #[test]
    fn no_amount_is_422() {
        let mut req = valid();
        req.images = None;
        let err = req.validate().unwrap_err();
        assert_eq!(err.len(), 1);
        assert_eq!(err.0[0].field, "images");
        assert_eq!(err.0[0].code, "missing_amount");
    }

    #[test]
    fn zero_or_negative_amount_is_422() {
        let mut req = valid();
        req.images = Some(0);
        let err = req.validate().unwrap_err();
        assert_eq!(err.len(), 1);
        assert_eq!(err.0[0].field, "images");
        let mut req = valid();
        req.video_seconds = Some(-30);
        let err = req.validate().unwrap_err();
        assert_eq!(err.len(), 1);
        assert_eq!(err.0[0].field, "video_seconds");
    }

    #[test]
    fn ttl_bounds_are_1_and_168() {
        let mut req = valid();
        req.ttl_hours = 0;
        assert!(req.validate().is_err());
        let mut req = valid();
        req.ttl_hours = 169;
        assert!(req.validate().is_err());
        let mut req = valid();
        req.ttl_hours = 1;
        assert!(req.validate().is_ok());
        let mut req = valid();
        req.ttl_hours = 168;
        assert!(req.validate().is_ok());
    }

    #[test]
    fn blank_reason_is_422() {
        let mut req = valid();
        req.reason = "   ".to_string();
        let err = req.validate().unwrap_err();
        assert_eq!(err.len(), 1);
        assert_eq!(err.0[0].field, "reason");
    }

    #[test]
    fn every_failure_is_collected_not_just_the_first() {
        // One round trip fixes the whole request (the
        // ValidatedJson contract): a request that is wrong in
        // three ways reports all three.
        let req = QuotaGrantRequest {
            images: Some(0),
            video_seconds: None,
            ttl_hours: 0,
            reason: String::new(),
        };
        let err = req.validate().unwrap_err();
        assert_eq!(err.len(), 3);
    }
}
