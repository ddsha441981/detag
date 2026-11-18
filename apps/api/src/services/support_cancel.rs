//! Support-facing immediate cancel (P1-071, FR-P-03):
//! cancel a subscription NOW, with the refund guidance and
//! the ToS policy snippet in the response.
//!
//! The endpoint is the admin billing write
//! `POST /v1/admin/billing/subscriptions/{id}/cancel` —
//! admin only (support stays read-only per FR-A-07; the
//! "support-facing" in the spec describes the workflow, not
//! a new role). The cancel is a move through the P1-064
//! state machine ([`apply_transition_as`], actor the
//! calling admin, `reason` the support note), so the row,
//! the `billing.subscription.status_changed` audit row, and
//! the `subscription-canceled` mail land together, exactly
//! like a webhook cancel — except the audit names the admin
//! (`actor_user_id` the admin, `actor_client` `"admin"`),
//! which is what distinguishes the two in the audit viewer.
//!
//! # What "immediate" means
//!
//! The row moves to `canceled` NOW: `cancel_at_period_end`
//! is cleared, `current_period_end` is kept as the
//! historical record of when the paid period would have
//! ended. This is the opposite timing from P1-069's
//! downgrade-to-Free, which keeps the row live until the
//! period end and only then cancels it. A row that is
//! already `canceled` is a 200 no-op (`changed: false` —
//! the machine's no-op precedent): no second audit row, no
//! second mail.
//!
//! A pending scheduled plan change (P1-069) for the
//! subscription is mooted right after the cancel commits:
//! it closes as `canceled`, so the period roll can never
//! apply a downgrade to a canceled row (the applier's moot
//! precedent — a canceled row moves nothing there either).
//!
//! # What this does NOT do
//!
//! It makes NO provider call: the Stripe subscription is
//! NOT canceled here and no refund is issued — there is no
//! refund API and no `cancel_subscription` leg on the
//! `BillingProvider` trait (ASSUMPTION-034). The response
//! carries the refund guidance instead: the Stripe
//! dashboard URL for the subscription plus a note telling
//! support to cancel it there too (so billing stops) and to
//! issue any refund on its latest invoice or payment. The
//! ToS policy snippet rides the response as `policy` — one
//! const here, quoted verbatim in the pricing-config doc.
//!
//! No idempotency header: the cancel is idempotent by
//! nature (same-state is a no-op), so a retry returns the
//! same 200 with `changed: false`.

use crate::AppState;
use detag_billing::SubscriptionStatus;
use detag_core::{
    error::AppError,
    repos::{plan_changes, subscriptions, users::User},
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use super::subscription_state::{apply_transition_as, AuditActor};

/// The ToS policy snippet (P1-071, ADR-0036): the canonical
/// text — returned as `policy` on every cancel response and
/// quoted verbatim in `docs/06-DELIVERY/10-pricing-config.md`.
/// One const, two surfaces, so the API and the doc can never
/// disagree.
pub const REFUND_CANCEL_POLICY: &str = "Cancelling ends the subscription immediately: paid access stops at once. No prorated refund is issued automatically — refunds are at support's discretion and are issued in the Stripe dashboard. This endpoint stops access; it does not cancel the Stripe subscription or move money — cancel it in the dashboard too, so billing stops.";

/// The refund-guidance note: where the money actions live.
/// Refunds are dashboard-only — this API never moves money.
pub const REFUND_GUIDANCE_NOTE: &str = "Refunds are issued in the Stripe dashboard, not by this API: open the subscription below, cancel it there too so billing stops, and issue any refund on its latest invoice or payment.";

/// The Stripe dashboard subscriptions list — the fallback
/// when the row carries no provider subscription id.
pub const STRIPE_DASHBOARD_SUBSCRIPTIONS_URL: &str = "https://dashboard.stripe.com/subscriptions";

/// The dashboard URL for one subscription: the subscription
/// page when we know its provider id, the list page when we
/// do not — never a broken link.
pub fn refund_dashboard_url(provider_subscription_id: Option<&str>) -> String {
    match provider_subscription_id {
        Some(id) => format!("{STRIPE_DASHBOARD_SUBSCRIPTIONS_URL}/{id}"),
        None => STRIPE_DASHBOARD_SUBSCRIPTIONS_URL.to_string(),
    }
}

/// Where a refund happens: the Stripe dashboard, never this
/// API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RefundGuidance {
    /// The Stripe dashboard URL for the subscription (or
    /// the subscriptions list when the row carries no
    /// provider id).
    pub dashboard_url: String,
    /// What support does there: cancel the Stripe
    /// subscription too, and issue any refund on the latest
    /// invoice or payment.
    pub note: String,
}

/// The 200 body for a support cancel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SupportCancelResponse {
    /// The subscription that was canceled (or already was).
    pub subscription_id: Uuid,
    /// The subscription's owner.
    pub user_id: Uuid,
    /// The row's status now — always `"canceled"`.
    pub status: String,
    /// The plan the subscription was on.
    pub plan_code: String,
    /// Whether this call moved the row (`false` = it was
    /// already canceled: no write, no audit, no mail).
    pub changed: bool,
    /// The ToS policy snippet ([`REFUND_CANCEL_POLICY`]).
    pub policy: String,
    /// Where the refund happens ([`RefundGuidance`]).
    pub refund_guidance: RefundGuidance,
}

/// What [`support_cancel`] can fail with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SupportCancelError {
    /// No subscription row with that id.
    NotFound,
    /// The machine rejected the move — only reachable on a
    /// lost race (a concurrent transition won between the
    /// load and the guarded write); the caller re-reads.
    Conflict,
    /// The database, or a row whose user/plan is gone.
    Internal,
}

/// Cancel a subscription immediately, as the calling admin
/// (P1-071). `reason` is the support note (required by the
/// handler's `ReasonRequest`); it lands on the audit row
/// verbatim. The pending scheduled change, if any, is mooted
/// after the cancel commits.
pub async fn support_cancel(
    state: &AppState,
    admin: &User,
    subscription_id: Uuid,
    reason: &str,
) -> Result<SupportCancelResponse, SupportCancelError> {
    let row = subscriptions::by_id(&state.pool, subscription_id)
        .await
        .map_err(|_| SupportCancelError::Internal)?
        .ok_or(SupportCancelError::NotFound)?;
    let plan = subscriptions::plan_for_subscription(&state.pool, row.plan_id)
        .await
        .map_err(|_| SupportCancelError::Internal)?
        .ok_or(SupportCancelError::Internal)?;
    let applied = apply_transition_as(
        state,
        subscription_id,
        SubscriptionStatus::Canceled,
        reason,
        AuditActor::admin(admin.id),
    )
    .await
    .map_err(|err| match err {
        AppError::InvalidStateTransition => SupportCancelError::Conflict,
        _ => SupportCancelError::Internal,
    })?;
    // Moot the pending scheduled change, if one exists: the
    // row is canceled now, so a later period roll must not
    // apply its downgrade. A `false` here only means a
    // concurrent applier closed it first — the change is
    // closed either way, so there is nothing to re-do. This
    // runs after the cancel commits (the applier moots a
    // canceled row the same way, so the order cannot strand
    // a live change against a dead row).
    if let Some(change) = plan_changes::pending_for_subscription(&state.pool, subscription_id)
        .await
        .map_err(|_| SupportCancelError::Internal)?
    {
        plan_changes::set_status_guarded(&state.pool, change.id, "canceled")
            .await
            .map_err(|_| SupportCancelError::Internal)?;
    }
    Ok(SupportCancelResponse {
        subscription_id,
        user_id: row.user_id,
        status: SubscriptionStatus::Canceled.as_db_str().to_string(),
        plan_code: plan.code,
        changed: applied.changed,
        policy: REFUND_CANCEL_POLICY.to_string(),
        refund_guidance: RefundGuidance {
            dashboard_url: refund_dashboard_url(row.provider_subscription_id.as_deref()),
            note: REFUND_GUIDANCE_NOTE.to_string(),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dashboard_url_uses_the_provider_id() {
        assert_eq!(
            refund_dashboard_url(Some("sub_123")),
            "https://dashboard.stripe.com/subscriptions/sub_123"
        );
    }

    #[test]
    fn dashboard_url_without_provider_id_is_the_list() {
        assert_eq!(
            refund_dashboard_url(None),
            "https://dashboard.stripe.com/subscriptions"
        );
    }
}
