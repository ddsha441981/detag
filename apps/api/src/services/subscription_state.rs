//! Subscription state machine — the service side (P1-064, FR-P-04):
//! apply a validated transition to a `subscriptions` row, with
//! its side effects.
//!
//! The pure rules live in `detag_billing::state_machine`
//! (`transition` / `target_for_event`); this service is the
//! reusable layer P1-065's webhook handlers call:
//!
//! * [`apply_transition`] — move one row to `to`, from whatever
//!   state it is in now. The move is validated by the pure
//!   machine first, then applied as a guarded
//!   `UPDATE ... WHERE status = $from` inside one transaction
//!   with the audit row and the lifecycle mail — a status change
//!   never lands without its trail, and two handlers racing the
//!   same row cannot both apply (the loser gets
//!   `InvalidStateTransition` and re-reads).
//! * [`apply_domain_event`] — the P1-065 entry point: map a
//!   [`DomainEvent`] to its target status (`None` = no state
//!   change — checkout creation and invoice payment are not
//!   transitions) and apply it the same way.
//!
//! Side effects:
//!
//! **Entitlement cache flush** — `state.plans.invalidate()`
//! after the transaction commits. There is no per-user
//! entitlement cache to flush: `effective_plan` (P1-049) reads
//! the subscription row live and the plan catalog cached;
//! invalidating the shared catalog is the flush, and it runs
//! after commit so a rolled-back move never flushes a cache
//! that still matches the DB. The next resolution re-reads
//! the plans and sees the new status immediately.
//!
//! **Lifecycle mail** — one outbox row in the same
//! transaction, template by [`mail_template_for_transition`]:
//! `trialing → active` welcomes, any move to `past_due`
//! duns, any move to `canceled` says goodbye. Recovery
//! (`past_due → active`) mails nothing — the dunning mail
//! already went out, and the spec names no recovery mail.
//! The payload is `{user_id, plan_code}` (ids only, NFR-42);
//! the address is the user's address on record, hashed.
//!
//! **Dunning state** — the same guarded write stamps the
//! row's `past_due_since` / `dunning_step` (P1-070,
//! ADR-0035): into `past_due` stamps the entry time and
//! resets the step to 0; out of `past_due` clears both, so
//! recovery resets the dunning sequence the policy job
//! reads.
//!
//! **Audit** — `billing.subscription.status_changed` in the
//! same transaction, `before` / `after` the two statuses, the
//! plan code in `after`, `reason` the caller's trigger (a
//! webhook event id, for P1-065). The actor is the provider:
//! `actor_user_id` is `None`, `actor_client` is `"webhook"`.
//!
//! A same-state ask is a no-op: no write, no audit, no mail
//! (`changed: false`) — a replayed webhook event has a single
//! effect (P1-066 leans on this). An illegal move is
//! [`AppError::InvalidStateTransition`] (PF-5002, 409); an
//! unknown row is [`AppError::NotFound`] (PF-2010, 404).
//!
//! No route calls this service yet — P1-065 wires the webhook
//! handlers to [`apply_domain_event`]. No Stripe key is needed:
//! the machine proves itself against seeded rows, like
//! P1-059–P1-063 did with fakes and live DB tests.

use crate::AppState;
use chrono::{DateTime, Utc};
use detag_billing::{target_for_event, transition, DomainEvent, SubscriptionStatus};
use detag_core::{
    checksum::sha256_hex,
    db::PoolConfig,
    error::AppError,
    repos::{
        audit_logs::{self, NewAuditLog},
        email_outbox::{self, NewOutboxMail},
        subscriptions::{self, Subscription},
        users, RepoError,
    },
    tx::{self, TxError},
};
use uuid::Uuid;

/// The audit action for every applied transition.
pub const SUBSCRIPTION_STATUS_CHANGED_ACTION: &str = "billing.subscription.status_changed";

/// The outbox template for the welcome mail
/// (`trialing → active`).
pub const SUBSCRIPTION_WELCOME_TEMPLATE: &str = "subscription-welcome";
/// The outbox template for the dunning mail (any move to
/// `past_due`).
pub const SUBSCRIPTION_DUNNING_TEMPLATE: &str = "subscription-dunning";
/// The outbox template for the cancellation mail (any move to
/// `canceled`).
pub const SUBSCRIPTION_CANCELED_TEMPLATE: &str = "subscription-canceled";

/// Which lifecycle mail a transition sends, if any. Pure —
///
/// * `trialing → active` welcomes (the trial converted);
/// * any move to `past_due` duns;
/// * any move to `canceled` says goodbye;
/// * everything else (including recovery `past_due → active`)
///   mails nothing.
pub fn mail_template_for_transition(
    from: SubscriptionStatus,
    to: SubscriptionStatus,
) -> Option<&'static str> {
    match (from, to) {
        (SubscriptionStatus::Trialing, SubscriptionStatus::Active) => {
            Some(SUBSCRIPTION_WELCOME_TEMPLATE)
        }
        (SubscriptionStatus::Trialing, SubscriptionStatus::PastDue)
        | (SubscriptionStatus::Active, SubscriptionStatus::PastDue) => {
            Some(SUBSCRIPTION_DUNNING_TEMPLATE)
        }
        (
            SubscriptionStatus::Trialing | SubscriptionStatus::Active | SubscriptionStatus::PastDue,
            SubscriptionStatus::Canceled,
        ) => Some(SUBSCRIPTION_CANCELED_TEMPLATE),
        _ => None,
    }
}

/// What [`apply_transition`] / [`apply_domain_event`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedTransition {
    /// The row that moved (or already held the target state).
    pub subscription_id: Uuid,
    /// The state the row was in.
    pub from: SubscriptionStatus,
    /// The state the row is in now (`from` when `changed` is false).
    pub to: SubscriptionStatus,
    /// Whether anything was written. `false` = the row already
    /// held the target state: no write, no audit, no mail.
    pub changed: bool,
    /// The lifecycle mail enqueued, if the move sends one.
    pub mail_template: Option<&'static str>,
}

/// The transaction closure's error: small on purpose — it must
/// not recurse into [`AppError`].
#[derive(Debug)]
enum TransitionTxError {
    Db(RepoError),
    /// The guarded update moved no row: a concurrent transition
    /// won the race between the load and the update.
    LostRace,
}

/// Move one subscription row to `to`, keeping its stored
/// `current_period_end` / `cancel_at_period_end` (the caller
/// with fresher provider values uses [`apply_domain_event`]).
/// `reason` names the trigger (a webhook event id, for P1-065)
/// and is stored on the audit row.
pub async fn apply_transition(
    state: &AppState,
    subscription_id: Uuid,
    to: SubscriptionStatus,
    reason: &str,
) -> Result<AppliedTransition, AppError> {
    let row = load_row(state, subscription_id).await?;
    apply_validated(
        state,
        &row,
        to,
        row.current_period_end,
        row.cancel_at_period_end,
        reason,
    )
    .await
}

/// The P1-065 entry point: map a [`DomainEvent`] to its target
/// status and apply it to the row P1-065 resolved (by provider
/// subscription id). `None`-target events (checkout creation,
/// invoice payment) are a no-op `Ok` with `changed: false` —
/// they are not transitions, and the handler must not fail on
/// them. `reason` is the webhook event id, stored on the audit
/// row when a move lands.
pub async fn apply_domain_event(
    state: &AppState,
    subscription_id: Uuid,
    event: &DomainEvent,
    reason: &str,
) -> Result<AppliedTransition, AppError> {
    let Some(to) = target_for_event(event) else {
        let row = load_row(state, subscription_id).await?;
        let from = parse_status(&row)?;
        return Ok(AppliedTransition {
            subscription_id,
            from,
            to: from,
            changed: false,
            mail_template: None,
        });
    };
    let row = load_row(state, subscription_id).await?;
    let (period_end, cancel_at_end) = match event {
        DomainEvent::SubscriptionStatusChanged {
            current_period_end,
            cancel_at_period_end,
            ..
        } => (*current_period_end, *cancel_at_period_end),
        _ => (row.current_period_end, row.cancel_at_period_end),
    };
    apply_validated(state, &row, to, period_end, cancel_at_end, reason).await
}

async fn load_row(state: &AppState, id: Uuid) -> Result<Subscription, AppError> {
    subscriptions::by_id(&state.pool, id)
        .await
        .map_err(|_| AppError::Internal)?
        .ok_or(AppError::NotFound)
}

fn parse_status(row: &Subscription) -> Result<SubscriptionStatus, AppError> {
    SubscriptionStatus::from_db_str(&row.status).map_err(|_| {
        tracing::error!(
            subscription_id = %row.id,
            status = %row.status,
            "subscription state: row carries a status outside the CHECK"
        );
        AppError::Internal
    })
}

#[allow(clippy::too_many_arguments)]
async fn apply_validated(
    state: &AppState,
    row: &Subscription,
    to: SubscriptionStatus,
    period_end: Option<DateTime<Utc>>,
    cancel_at_period_end: bool,
    reason: &str,
) -> Result<AppliedTransition, AppError> {
    let from = parse_status(row)?;
    let outcome = transition(from, to).map_err(|_| AppError::InvalidStateTransition)?;
    if !outcome.changed {
        return Ok(AppliedTransition {
            subscription_id: row.id,
            from,
            to,
            changed: false,
            mail_template: None,
        });
    }
    let mail_template = mail_template_for_transition(from, to);
    // The mail + audit need the user and the plan: load them
    // before the transaction (reads, never part of the write).
    let user = users::by_id(&state.pool, row.user_id)
        .await
        .map_err(|_| AppError::Internal)?
        .ok_or(AppError::Internal)?;
    let plan = subscriptions::plan_for_subscription(&state.pool, row.plan_id)
        .await
        .map_err(|_| AppError::Internal)?
        .ok_or(AppError::Internal)?;
    // Dunning state (P1-070, ADR-0035) rides the same guarded
    // write as the status, so the two can never disagree: a
    // move INTO `past_due` stamps the entry time (app clock)
    // and resets the step to 0 — the day-0 mail this move
    // sends; a move OUT of `past_due` clears both, so recovery
    // resets the sequence; any other move carries the row's
    // own values (NULL/0 in practice — only a past_due row
    // carries dunning state).
    let now = state.clock.now();
    let (past_due_since, dunning_step) = if to == SubscriptionStatus::PastDue {
        (Some(now), 0)
    } else if from == SubscriptionStatus::PastDue {
        (None, 0)
    } else {
        (row.past_due_since, row.dunning_step)
    };
    let timeout = PoolConfig::default().transaction_timeout;
    let from_str = from.as_db_str().to_string();
    let to_str = to.as_db_str().to_string();
    let reason_owned = reason.to_string();
    let user_email = user.email.clone();
    let user_id = user.id;
    let plan_code = plan.code.clone();
    let subscription_id = row.id;
    tx::with_tx(&state.pool, timeout, |conn| {
        let from_str = from_str.clone();
        let to_str = to_str.clone();
        let reason_owned = reason_owned.clone();
        let user_email = user_email.clone();
        let plan_code = plan_code.clone();
        Box::pin(async move {
            let moved = subscriptions::set_status_guarded(
                &mut *conn,
                subscription_id,
                &from_str,
                &to_str,
                period_end,
                cancel_at_period_end,
                past_due_since,
                dunning_step,
            )
            .await
            .map_err(TransitionTxError::Db)?;
            if !moved {
                return Err(TransitionTxError::LostRace);
            }
            audit_logs::insert(
                &mut *conn,
                &NewAuditLog {
                    actor_user_id: None,
                    actor_client: Some("webhook".to_string()),
                    action: SUBSCRIPTION_STATUS_CHANGED_ACTION.to_string(),
                    target_type: "subscription".to_string(),
                    target_id: Some(subscription_id.to_string()),
                    before: Some(serde_json::json!({ "status": from_str })),
                    after: Some(serde_json::json!({
                        "status": to_str,
                        "plan_code": plan_code,
                    })),
                    reason: Some(reason_owned),
                },
            )
            .await
            .map_err(TransitionTxError::Db)?;
            if let Some(template) = mail_template {
                email_outbox::enqueue(
                    &mut *conn,
                    &NewOutboxMail {
                        to_hash: sha256_hex(user_email.as_bytes()),
                        template: template.to_string(),
                        payload: serde_json::json!({
                            "user_id": user_id,
                            "plan_code": plan_code,
                        }),
                        scheduled_at: None,
                    },
                )
                .await
                .map_err(TransitionTxError::Db)?;
            }
            Ok(())
        })
    })
    .await
    .map_err(|err| match err {
        TxError::Inner(TransitionTxError::LostRace) => AppError::InvalidStateTransition,
        TxError::Inner(TransitionTxError::Db(db_err)) => {
            tracing::error!(error = ?db_err, "subscription state: transaction failed (db)");
            AppError::Internal
        }
        other => {
            tracing::error!(error = ?other, "subscription state: transaction failed");
            AppError::Internal
        }
    })?;
    // Flush AFTER commit: the next resolution re-reads the plans
    // and sees the new status; a rolled-back move never flushes
    // a cache that still matches the DB.
    state.plans.invalidate();
    Ok(AppliedTransition {
        subscription_id,
        from,
        to,
        changed: true,
        mail_template,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use detag_billing::SubscriptionStatus::*;

    #[test]
    fn mail_mapping() {
        assert_eq!(
            mail_template_for_transition(Trialing, Active),
            Some("subscription-welcome")
        );
        assert_eq!(
            mail_template_for_transition(Trialing, PastDue),
            Some("subscription-dunning")
        );
        assert_eq!(
            mail_template_for_transition(Active, PastDue),
            Some("subscription-dunning")
        );
        assert_eq!(
            mail_template_for_transition(Active, Canceled),
            Some("subscription-canceled")
        );
        assert_eq!(
            mail_template_for_transition(PastDue, Canceled),
            Some("subscription-canceled")
        );
        assert_eq!(mail_template_for_transition(PastDue, Active), None);
        assert_eq!(mail_template_for_transition(Active, Active), None);
    }
}
