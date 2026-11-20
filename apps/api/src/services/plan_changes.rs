//! Scheduled plan changes (P1-069, FR-P-03): the applier.
//!
//! A downgrade is scheduled by the checkout endpoint as a
//! `scheduled_plan_changes` row (user, subscription, from/to
//! plan, `effective_at` = the period end) — no provider call,
//! no charge. This service is what flips the entitlement when
//! the period rolls: the webhook handler calls it after
//! applying a `SubscriptionStatusChanged` (or any event that
//! resolves to the row — an `invoice.paid` for the new period
//! is the same evidence), and it applies the pending change
//! exactly once.
//!
//! # When a change is due
//!
//! Either signal suffices (both are honest evidence the
//! period rolled):
//! * the app clock reached `effective_at` (`now >=
//!   effective_at`), or
//! * the event carries a period end past `effective_at`
//!   (the provider already started the next period).
//!
//! The second signal matters on a frozen clock and when the
//! webhook arrives late; the first covers a roll whose event
//! never carried a new period end.
//!
//! # What applying does, in one transaction
//!
//! * A paid target: the subscription row moves to the target
//!   plan (`set_plan_guarded` — only while the row still
//!   grants a plan, `trialing`/`active`).
//! * A Free target (amount 0): the row is canceled — there
//!   is no Free subscription row to move to, and the user
//!   falls back to Free the moment no live row exists
//!   (P1-049). The cancel is the guarded
//!   `active → canceled` move, so a row that already ended
//!   is untouched.
//! * The change row closes as `applied`, and a
//!   `billing.subscription.plan_changed` audit row lands in
//!   the same transaction — a plan switch never lands
//!   without its trail.
//! * A row that no longer grants a plan makes the change
//!   moot: the change closes as `canceled` instead, with no
//!   audit row — the subscription ended first, and there is
//!   nothing to switch.
//!
//! The catalog flushes after commit (the P1-064 rule): the
//! next `effective_plan` resolution sees the new plan.
//!
//! # What this does NOT do
//!
//! It never calls the provider. The provider-side price
//! switch (Stripe subscription schedule / cancel at period
//! end) is a follow-up (ASSUMPTION-032): our DB is the
//! entitlement source of truth, and the switch cannot be
//! proven here with no test-mode key. A failed apply is
//! [`AppError::Internal`] — the webhook answers 500, Stripe
//! retries, and the retry is a no-op (the change row is no
//! longer `scheduled`).

use crate::AppState;
use chrono::{DateTime, Utc};
use detag_core::{
    audit::{record, NewAuditLog},
    db::PoolConfig,
    error::AppError,
    repos::{
        plan_changes::{self, ScheduledPlanChange},
        plans, subscriptions,
    },
    tx::{self, TxError},
};
use uuid::Uuid;

/// The audit action for a plan switch applied at a period
/// boundary.
pub const PLAN_CHANGED_ACTION: &str = "billing.subscription.plan_changed";

/// What [`apply_due_changes`] did with one subscription.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyOutcome {
    /// No change was pending for this subscription.
    None,
    /// A change is pending but not due yet.
    NotDue {
        /// When it comes due.
        effective_at: DateTime<Utc>,
    },
    /// The change was applied: the row is on the target plan
    /// (or canceled for a Free target), the change row is
    /// `applied`, and the audit row is written.
    Applied {
        /// The subscription row that moved.
        subscription_id: Uuid,
        /// The plan code it moved to (`free` for a cancel).
        to_plan_code: String,
    },
    /// The subscription no longer grants a plan, so the
    /// pending change was closed as `canceled` — moot.
    Moot {
        /// The subscription row that had already ended.
        subscription_id: Uuid,
    },
}

/// The transaction closure's error: small on purpose — it must
/// not recurse into [`AppError`].
#[derive(Debug)]
enum ApplyTxError {
    Db(detag_core::repos::RepoError),
    /// A concurrent applier closed the change first: the
    /// guarded subscription write above already moved
    /// nothing twice — the loser rolls back and re-reads
    /// instead of applying twice.
    LostRace,
}

/// Apply the subscription's pending scheduled change, if one
/// is due. `period_end_evidence` is the period end the
/// webhook event just reported, when it reported one
/// (`SubscriptionStatusChanged` carries it; an invoice event
/// does not). `event_id` lands on the audit row as the
/// reason. See the module docs for what "due" means.
pub async fn apply_due_changes(
    state: &AppState,
    subscription_id: Uuid,
    period_end_evidence: Option<DateTime<Utc>>,
    event_id: &str,
) -> Result<ApplyOutcome, AppError> {
    let row = subscriptions::by_id(&state.pool, subscription_id)
        .await
        .map_err(|_| AppError::Internal)?
        .ok_or(AppError::NotFound)?;
    let Some(change) = plan_changes::pending_for_subscription(&state.pool, subscription_id)
        .await
        .map_err(|_| AppError::Internal)?
    else {
        return Ok(ApplyOutcome::None);
    };
    let now = state.clock.now();
    let due = now >= change.effective_at
        || period_end_evidence
            .map(|end| end > change.effective_at)
            .unwrap_or(false);
    if !due {
        return Ok(ApplyOutcome::NotDue {
            effective_at: change.effective_at,
        });
    }
    let target = plans::by_id(&state.pool, change.to_plan_id)
        .await
        .map_err(|_| AppError::Internal)?
        .ok_or(AppError::Internal)?;
    let from_plan = plans::by_id(&state.pool, change.from_plan_id)
        .await
        .map_err(|_| AppError::Internal)?
        .ok_or(AppError::Internal)?;
    apply_change(state, &row, &change, &target, from_plan.code, event_id).await
}

async fn apply_change(
    state: &AppState,
    row: &subscriptions::Subscription,
    change: &ScheduledPlanChange,
    target: &plans::Plan,
    from_plan_code: String,
    event_id: &str,
) -> Result<ApplyOutcome, AppError> {
    let timeout = PoolConfig::default().transaction_timeout;
    let to_free = target.amount == 0;
    let from_status = row.status.clone();
    let period_end = row.current_period_end;
    let subscription_id = row.id;
    let change_id = change.id;
    let effective_at = change.effective_at;
    let target_plan_id = target.id;
    let to_plan_code = target.code.clone();
    let reason = event_id.to_string();
    let applied = match tx::with_tx(&state.pool, timeout, |conn| {
        let from_status = from_status.clone();
        let to_plan_code = to_plan_code.clone();
        let from_plan_code = from_plan_code.clone();
        let reason = reason.clone();
        Box::pin(async move {
            // The plan switch first: a row that no longer
            // grants a plan moves nothing, and the change is
            // moot instead of applied.
            let moved = if to_free {
                subscriptions::set_status_guarded(
                    &mut *conn,
                    subscription_id,
                    &from_status,
                    "canceled",
                    period_end,
                    false,
                    // Leaving any status for `canceled` clears
                    // the dunning state (P1-070): a canceled row
                    // carries no `past_due_since` by the CHECK.
                    None,
                    0,
                )
                .await
                .map_err(ApplyTxError::Db)?
            } else {
                subscriptions::set_plan_guarded(&mut *conn, subscription_id, target_plan_id)
                    .await
                    .map_err(ApplyTxError::Db)?
            };
            let close_as = if moved { "applied" } else { "canceled" };
            let closed = plan_changes::set_status_guarded(&mut *conn, change_id, close_as)
                .await
                .map_err(ApplyTxError::Db)?;
            if !closed {
                // A concurrent applier (or a mooting write)
                // closed the change first: the guarded
                // subscription write above already moved
                // nothing twice — roll back and let the
                // caller re-read the winner's outcome.
                return Err(ApplyTxError::LostRace);
            }
            if moved {
                record(
                    &mut *conn,
                    &NewAuditLog {
                        actor_user_id: None,
                        actor_client: Some("webhook".to_string()),
                        action: PLAN_CHANGED_ACTION.to_string(),
                        target_type: "subscription".to_string(),
                        target_id: Some(subscription_id.to_string()),
                        before: Some(serde_json::json!({ "plan_code": from_plan_code })),
                        after: Some(serde_json::json!({
                            "plan_code": to_plan_code,
                            "effective_at": effective_at,
                        })),
                        reason: Some(reason),
                    },
                )
                .await
                .map_err(ApplyTxError::Db)?;
            }
            Ok(moved)
        })
    })
    .await
    {
        Ok(moved) => Some(moved),
        Err(TxError::Inner(ApplyTxError::LostRace)) => {
            // The winner closed the change: re-read it and
            // report the winner's outcome — applied, or moot
            // if the winner found the subscription ended.
            // (A change that is somehow still scheduled here
            // is a bug, not a state: fail loudly so the
            // webhook retries.)
            let winner = plan_changes::by_id(&state.pool, change_id)
                .await
                .map_err(|_| AppError::Internal)?
                .ok_or(AppError::Internal)?;
            return Ok(match winner.status.as_str() {
                "applied" => ApplyOutcome::Applied {
                    subscription_id,
                    to_plan_code,
                },
                "canceled" => ApplyOutcome::Moot { subscription_id },
                _ => return Err(AppError::Internal),
            });
        }
        Err(TxError::Inner(ApplyTxError::Db(db_err))) => {
            tracing::error!(error = ?db_err, "plan changes: apply transaction failed");
            return Err(AppError::Internal);
        }
        Err(other) => {
            tracing::error!(error = ?other, "plan changes: apply transaction failed");
            return Err(AppError::Internal);
        }
    };
    let moved = applied.expect("the match above returns on every Err arm");
    // Flush AFTER commit: the next resolution re-reads the
    // plans and sees the new plan; a rolled-back move never
    // flushes a cache that still matches the DB.
    state.plans.invalidate();
    if moved {
        Ok(ApplyOutcome::Applied {
            subscription_id,
            to_plan_code,
        })
    } else {
        Ok(ApplyOutcome::Moot { subscription_id })
    }
}
