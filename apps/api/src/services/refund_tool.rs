//! Refund tool stub (P1-108, FR-C-02): `POST
//! /v1/admin/billing/subscriptions/{id}/refund` — the guided
//! checklist for refunding a subscription in the Stripe
//! dashboard.
//!
//! The tool guides a human through the dashboard refund; it
//! never calls Stripe and never moves money (ASSUMPTION-034
//! stands). The response carries the dashboard deep-link +
//! note (reused `RefundGuidance` + `refund_dashboard_url`
//! from [`support_cancel`](super::support_cancel) — no second
//! URL builder), a four-step checklist (all `done: false` —
//! the API cannot know what a human did in the dashboard),
//! and the required fields (values where known, `null`
//! where only support knows — `amount` and
//! `invoice_or_payment_id`, because the tool makes NO
//! provider read to find the latest invoice).
//!
//! One `billing.refund.requested` audit row per call (actor
//! the admin, target the subscription, reason verbatim,
//! after `{subscription_id, dashboard_url}`); every call
//! writes one row — a note per request, not idempotent (the
//! warn precedent). No state change, no provider call, no
//! mail. Any status may be refunded, including
//! already-canceled (the common case). The handler marks the
//! response `AuditHandled` so the P1-046 middleware stands
//! down and the row exists exactly once.
//!
//! Admin-only ([`AdminContext`] — support is read-only, so
//! support gets 403 `PF-1002` from the extractor, an API key
//! 403, no credential 401 `PF-1001`). A subscription that
//! does not exist is 404 `PF-2010` (the P1-071 precedent).
//! The handler answers 200 (an admin action, the
//! suspend/warn/cancel precedent), not 201.

use crate::AppState;
use detag_core::{
    audit::{record, NewAuditLog},
    repos::{subscriptions, users::User},
};
use detag_validator::{validate_reason, ValidationErrors};
use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;
use utoipa::ToSchema;
use uuid::Uuid;

use super::support_cancel::{refund_dashboard_url, RefundGuidance, REFUND_GUIDANCE_NOTE};

/// The audit action for a refund request (the matrix already
/// maps the route to it — `Capability::audit_action_for`).
pub const REFUND_REQUESTED_AUDIT_ACTION: &str = "billing.refund.requested";

/// What a refund request can fail with.
#[derive(Debug, Error)]
pub enum RefundToolError {
    #[error("refund request validation failed")]
    Validation(ValidationErrors),
    #[error("subscription not found")]
    NotFound,
    #[error("refund request failed")]
    Internal,
}

/// One checklist step: what support does in the dashboard,
/// in order. `done` is always `false` — the API cannot know
/// what a human did in the dashboard, and a `done: true`
/// would be a lie.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RefundChecklistStep {
    /// The step number, 1–4, in the order support does them.
    pub step: u8,
    /// What support does at this step.
    pub detail: String,
    /// Whether the step is done — always `false` (see above).
    pub done: bool,
}

/// One field the dashboard refund form needs: the value
/// where the API knows it, `null` where only support knows
/// it (a null with no guess is honest — a guessed id would
/// be a lie).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RefundRequiredField {
    /// The field name, as the dashboard form names it.
    pub field: String,
    /// The value, or `null` when only support knows it.
    pub value: Option<serde_json::Value>,
}

/// The 200 body for a refund request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RefundChecklistResponse {
    /// The subscription the refund is for.
    #[schema(value_type = String, example = "123e4567-e89b-42d3-a456-426614174109")]
    pub subscription_id: Uuid,
    /// The subscription's owner.
    #[schema(value_type = String, example = "123e4567-e89b-42d3-a456-426614174109")]
    pub user_id: Uuid,
    /// The row's status now — any status may be refunded,
    /// including already-canceled.
    pub status: String,
    /// The plan the subscription is on.
    pub plan_code: String,
    /// Where the refund happens ([`RefundGuidance`]).
    pub refund_guidance: RefundGuidance,
    /// The four dashboard steps, all `done: false`.
    pub checklist: Vec<RefundChecklistStep>,
    /// The fields the dashboard refund form needs.
    pub required_fields: Vec<RefundRequiredField>,
}

/// The four dashboard steps, in the order support does them
/// (ADR-0067 §3). Pure — the unit test pins the shape.
pub fn build_checklist() -> Vec<RefundChecklistStep> {
    vec![
        RefundChecklistStep {
            step: 1,
            detail: "Open the Stripe dashboard link for this subscription."
                .to_string(),
            done: false,
        },
        RefundChecklistStep {
            step: 2,
            detail: "Confirm the Stripe subscription state there — cancel it in the dashboard too if billing should stop. This tool never cancels anything: the cancel endpoint owns the Detag-side cancel."
                .to_string(),
            done: false,
        },
        RefundChecklistStep {
            step: 3,
            detail: "Issue the refund on the latest invoice or payment in the dashboard."
                .to_string(),
            done: false,
        },
        RefundChecklistStep {
            step: 4,
            detail: "Record the refunded amount and the dashboard receipt back in the support ticket — there is no field for it here."
                .to_string(),
            done: false,
        },
    ]
}

/// The fields the dashboard refund form needs: the values
/// where the row knows them, `null` where only support
/// knows them. Pure — the unit test pins the nulls.
pub fn build_required_fields(
    subscription_id: Uuid,
    provider_customer_id: Option<&str>,
    reason: &str,
) -> Vec<RefundRequiredField> {
    vec![
        RefundRequiredField {
            field: "subscription_id".to_string(),
            value: Some(json!(subscription_id.to_string())),
        },
        RefundRequiredField {
            field: "provider_customer_id".to_string(),
            value: provider_customer_id.map(|id| json!(id)),
        },
        RefundRequiredField {
            field: "reason".to_string(),
            value: Some(json!(reason)),
        },
        RefundRequiredField {
            field: "amount".to_string(),
            value: None,
        },
        RefundRequiredField {
            field: "invoice_or_payment_id".to_string(),
            value: None,
        },
    ]
}

/// Request a refund for a subscription (P1-108). No state
/// change, no provider call, no mail: the row is only read,
/// and exactly one `billing.refund.requested` audit row is
/// written (the reason trimmed, the P1-008 precedent — the
/// handler's `ReasonRequest` already validated it, this is
/// the transform that yields the trimmed reason to store).
pub async fn request_refund(
    state: &AppState,
    admin: &User,
    subscription_id: Uuid,
    reason: &str,
) -> Result<RefundChecklistResponse, RefundToolError> {
    let reason = validate_reason("reason", reason)
        .map_err(|err| RefundToolError::Validation(ValidationErrors(vec![err])))?;
    let row = subscriptions::by_id(&state.pool, subscription_id)
        .await
        .map_err(|err| {
            tracing::error!(error = ?err, "refund tool: subscription lookup failed");
            RefundToolError::Internal
        })?
        .ok_or(RefundToolError::NotFound)?;
    let plan = subscriptions::plan_for_subscription(&state.pool, row.plan_id)
        .await
        .map_err(|err| {
            tracing::error!(error = ?err, "refund tool: plan lookup failed");
            RefundToolError::Internal
        })?
        .ok_or(RefundToolError::Internal)?;
    let dashboard_url = refund_dashboard_url(row.provider_subscription_id.as_deref());
    record(
        &state.pool,
        &NewAuditLog {
            actor_user_id: Some(admin.id),
            actor_client: None,
            action: REFUND_REQUESTED_AUDIT_ACTION.to_string(),
            target_type: "subscription".to_string(),
            target_id: Some(subscription_id.to_string()),
            before: None,
            after: Some(json!({
                "subscription_id": subscription_id.to_string(),
                "dashboard_url": dashboard_url,
            })),
            reason: Some(reason.clone()),
        },
    )
    .await
    .map_err(|err| {
        tracing::error!(error = ?err, "refund tool: audit write failed");
        RefundToolError::Internal
    })?;
    Ok(RefundChecklistResponse {
        subscription_id: row.id,
        user_id: row.user_id,
        status: row.status.clone(),
        plan_code: plan.code.clone(),
        refund_guidance: RefundGuidance {
            dashboard_url,
            note: REFUND_GUIDANCE_NOTE.to_string(),
        },
        checklist: build_checklist(),
        required_fields: build_required_fields(
            row.id,
            row.provider_customer_id.as_deref(),
            &reason,
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checklist_has_four_steps_all_undone_in_order() {
        let steps = build_checklist();
        assert_eq!(steps.len(), 4, "the checklist is four steps");
        for (i, step) in steps.iter().enumerate() {
            assert_eq!(step.step, (i + 1) as u8, "steps are 1–4 in order");
            assert!(!step.done, "no step can be done by the API");
            assert!(!step.detail.is_empty(), "every step says what to do");
        }
        let details: Vec<&str> = steps.iter().map(|s| s.detail.as_str()).collect();
        let unique: std::collections::HashSet<&str> = details.iter().copied().collect();
        assert_eq!(unique.len(), 4, "every step is distinct");
        assert!(
            steps[0].detail.contains("dashboard link"),
            "step 1 opens the link: {}",
            steps[0].detail
        );
        assert!(
            steps[2].detail.contains("latest invoice"),
            "step 3 issues the refund: {}",
            steps[2].detail
        );
    }

    #[test]
    fn required_fields_carry_known_values_and_nulls_for_unknowns() {
        let id = Uuid::now_v7();
        let fields = build_required_fields(id, Some("cus_123"), "duplicate charge");
        assert_eq!(fields.len(), 5, "five fields: three known, two null");
        let by_name = |name: &str| {
            fields
                .iter()
                .find(|f| f.field == name)
                .unwrap_or_else(|| panic!("field {name} present"))
        };
        assert_eq!(
            by_name("subscription_id").value,
            Some(json!(id.to_string())),
            "subscription_id is the row's id"
        );
        assert_eq!(
            by_name("provider_customer_id").value,
            Some(json!("cus_123")),
            "provider_customer_id is the row's value"
        );
        assert_eq!(
            by_name("reason").value,
            Some(json!("duplicate charge")),
            "reason is verbatim"
        );
        assert_eq!(
            by_name("amount").value,
            None,
            "amount is null — only support knows it"
        );
        assert_eq!(
            by_name("invoice_or_payment_id").value,
            None,
            "invoice id is null — no provider read is made"
        );
        // A row with no provider customer id names no value
        // either — null, not a guessed id.
        let fields = build_required_fields(id, None, "duplicate charge");
        let by_name = |name: &str| {
            fields
                .iter()
                .find(|f| f.field == name)
                .unwrap_or_else(|| panic!("field {name} present"))
        };
        assert_eq!(
            by_name("provider_customer_id").value,
            None,
            "no provider customer id on the row means null"
        );
    }
}
