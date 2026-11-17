//! Webhook event handlers (P1-065, FR-P-04): apply a mapped
//! [`DomainEvent`] to the `subscriptions` table. The receiver
//! (P1-063, [`crate::services::webhook`]) verifies the signature,
//! stores the envelope, and maps the event; this service is what
//! the dispatch seam hands it to.
//!
//! # What each event does
//!
//! * `CheckoutCompleted` — creates the subscription row. The user
//!   comes off the event (`client_reference_id`, P1-060); the
//!   status, period end, and price ids come off a fresh
//!   `fetch_subscription` (the provider-reported state, ADR-0032);
//!   the plan is the catalog row whose `stripe_price_id` matches
//!   one of those prices. The insert and the `billing.subscription
//!   .created` audit row land in one transaction.
//! * `SubscriptionStatusChanged` / `SubscriptionDeleted` — resolve
//!   the row by provider subscription id and apply the move through
//!   [`subscription_state::apply_domain_event`](crate::services::subscription_state::apply_domain_event)
//!   (P1-064's service: guarded write + audit + lifecycle mail).
//! * `InvoicePaid` — no lifecycle effect by itself (a past-due
//!   recovery arrives as a `SubscriptionStatusChanged` to `active`
//!   alongside it, P1-064): the handler resolves the row and
//!   applies the event, which is a no-op for this variant.
//! * `InvoicePaymentFailed` — moves the linked subscription to
//!   `past_due` (the dunning trigger) through the same path; the
//!   state machine decides whether the row may move there.
//!
//! # What never fails the webhook
//!
//! The endpoint answers 200 for every verified delivery (ADR-0031),
//! so a handler outcome that is *about the event* is a [`Noop`],
//! never an error: an event that names no user, a subscription
//! the provider no longer knows, a price no plan carries, a row
//! that does not exist yet (a `subscription.updated` that arrived
//! before its `checkout.session.completed`), a one-off invoice,
//! or a move the state machine rejects (a stale failure for an
//! already-canceled row) is logged and answered 200 — the stored
//! envelope stays in the ledger for inspection, and the provider
//! stops retrying. Only a genuine failure — the database, or a
//! provider call that errored for a reason other than "unknown
//! subscription" — is [`AppError::Internal`], which the receiver
//! answers 500 so Stripe retries.
//!
//! Creation sends no lifecycle mail: the welcome mail fires on
//! the `trialing → active` transition (P1-064), and a row created
//! already-`active` is a purchase, not a conversion — a purchase
//! welcome is a product decision, not this ticket's (see
//! ASSUMPTION-028).

use crate::AppState;
use detag_billing::{BillingError, DomainEvent, DynBillingProvider};
use detag_core::{
    db::PoolConfig,
    error::AppError,
    repos::{
        audit_logs::{self, NewAuditLog},
        plans, subscriptions, users,
    },
    tx::{self, TxError},
};
use uuid::Uuid;

/// The audit action for a subscription row created by a webhook.
pub const SUBSCRIPTION_CREATED_ACTION: &str = "billing.subscription.created";

/// What [`handle_domain_event`] did with one mapped event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandlerOutcome {
    /// A subscription row was created (checkout). The id is ours.
    Created {
        /// The new row's id.
        subscription_id: Uuid,
    },
    /// A transition was applied to an existing row (or the row
    /// already held the target state — `changed: false`).
    Transitioned {
        /// The row that moved (or already held the target state).
        subscription_id: Uuid,
        /// Whether anything was written.
        changed: bool,
    },
    /// Nothing was written. `reason` is a static sentence for
    /// the log — the event was processed, the provider stops
    /// retrying, and the envelope stays in the ledger.
    Noop {
        /// Why nothing was written.
        reason: &'static str,
    },
}

fn noop(reason: &'static str) -> HandlerOutcome {
    HandlerOutcome::Noop { reason }
}

/// The transaction closure's error: small on purpose — it must
/// not recurse into [`AppError`].
#[derive(Debug)]
enum CreateTxError {
    Db(detag_core::repos::RepoError),
}

/// Apply one mapped domain event. `provider` is the configured
/// provider (for the checkout's `fetch_subscription`);
/// `event_id` is the provider's event id, stored on the audit
/// row a write creates. See the module docs for what is a
/// [`HandlerOutcome::Noop`] and what is an error.
pub async fn handle_domain_event(
    state: &AppState,
    provider: &DynBillingProvider,
    event: &DomainEvent,
    event_id: &str,
) -> Result<HandlerOutcome, AppError> {
    match event {
        DomainEvent::CheckoutCompleted {
            customer_id,
            subscription_id,
            user_id,
        } => {
            create_subscription(
                state,
                provider,
                customer_id,
                subscription_id,
                *user_id,
                event_id,
            )
            .await
        }
        DomainEvent::SubscriptionStatusChanged {
            subscription_id, ..
        }
        | DomainEvent::SubscriptionDeleted { subscription_id } => {
            apply_to_subscription(
                state,
                provider.provider_name(),
                subscription_id,
                event,
                event_id,
            )
            .await
        }
        DomainEvent::InvoicePaid {
            subscription_id, ..
        }
        | DomainEvent::InvoicePaymentFailed {
            subscription_id, ..
        } => match subscription_id {
            None => {
                tracing::info!(
                    event_id = event_id,
                    "webhook handler: one-off invoice has no subscription"
                );
                Ok(noop("one-off invoice has no subscription"))
            }
            Some(subscription_id) => {
                apply_to_subscription(
                    state,
                    provider.provider_name(),
                    subscription_id,
                    event,
                    event_id,
                )
                .await
            }
        },
    }
}

/// Resolve an event's subscription row and apply the event to
/// it through the state machine's service. A missing row, or a
/// move the machine rejects, is a [`HandlerOutcome::Noop`] —
/// the event is stale or early, not failed (see module docs).
async fn apply_to_subscription(
    state: &AppState,
    provider_name: &str,
    provider_subscription_id: &str,
    event: &DomainEvent,
    event_id: &str,
) -> Result<HandlerOutcome, AppError> {
    let row = subscriptions::by_provider_id(&state.pool, provider_name, provider_subscription_id)
        .await
        .map_err(|_| AppError::Internal)?;
    let Some(row) = row else {
        tracing::info!(
            event_id = event_id,
            provider_subscription_id = provider_subscription_id,
            "webhook handler: no subscription row for this event"
        );
        // Deliberately NOT an error: the row is created by
        // `checkout.session.completed`, which may not have
        // arrived (or been applied) yet.
        return Ok(noop("no subscription row for this event"));
    };
    let applied =
        crate::services::subscription_state::apply_domain_event(state, row.id, event, event_id)
            .await;
    match applied {
        Ok(applied) => Ok(HandlerOutcome::Transitioned {
            subscription_id: applied.subscription_id,
            changed: applied.changed,
        }),
        Err(AppError::InvalidStateTransition) => {
            tracing::info!(
                event_id = event_id,
                provider_subscription_id = provider_subscription_id,
                "webhook handler: state machine rejected the move"
            );
            Ok(noop("state machine rejected the move"))
        }
        Err(AppError::NotFound) => Ok(noop("no subscription row for this event")),
        Err(err) => Err(err),
    }
}

/// Create the subscription row for a completed checkout.
///
/// Resolution, in order: the user (from the event — a checkout
/// with no user cannot be linked and is logged); the
/// provider-reported subscription state (a subscription the
/// provider no longer knows is logged — it was deleted between
/// checkout and delivery); the plan (from the subscription's
/// price ids — a price no plan carries is config drift, logged,
/// not invented); the user row (a user deleted between checkout
/// and delivery is logged). An already-recorded subscription
/// (a redelivered create that reached this far) is a no-op:
/// exactly one row per provider subscription.
async fn create_subscription(
    state: &AppState,
    provider: &DynBillingProvider,
    customer_id: &str,
    provider_subscription_id: &str,
    user_id: Option<Uuid>,
    event_id: &str,
) -> Result<HandlerOutcome, AppError> {
    let Some(user_id) = user_id else {
        tracing::info!(
            event_id = event_id,
            "webhook handler: checkout names no user"
        );
        return Ok(noop("checkout names no user"));
    };
    let fetched = provider.fetch_subscription(provider_subscription_id).await;
    let fetched = match fetched {
        Ok(fetched) => fetched,
        Err(BillingError::UnknownSubscription(_)) => {
            tracing::info!(
                event_id = event_id,
                provider_subscription_id = provider_subscription_id,
                "webhook handler: provider no longer knows this subscription"
            );
            return Ok(noop("provider no longer knows this subscription"));
        }
        Err(err) => {
            tracing::error!(error = %err, "webhook handler: fetch_subscription failed");
            return Err(AppError::Internal);
        }
    };
    let user_exists = users::by_id(&state.pool, user_id)
        .await
        .map_err(|_| AppError::Internal)?
        .is_some();
    if !user_exists {
        tracing::info!(
            event_id = event_id,
            "webhook handler: checkout user no longer exists"
        );
        return Ok(noop("checkout user no longer exists"));
    }
    let mut plan = None;
    for price_id in &fetched.price_ids {
        if let Some(found) = plans::by_stripe_price_id(&state.pool, price_id)
            .await
            .map_err(|_| AppError::Internal)?
        {
            plan = Some(found);
            break;
        }
    }
    let Some(plan) = plan else {
        tracing::info!(
            event_id = event_id,
            provider_subscription_id = provider_subscription_id,
            "webhook handler: no plan carries this subscription's price"
        );
        return Ok(noop("no plan carries this subscription's price"));
    };
    if subscriptions::by_provider_id(
        &state.pool,
        provider.provider_name(),
        provider_subscription_id,
    )
    .await
    .map_err(|_| AppError::Internal)?
    .is_some()
    {
        return Ok(noop("subscription already recorded"));
    }
    // One transaction: the row and its audit trail land
    // together — a subscription is never recorded without the
    // `billing.subscription.created` row that explains it.
    let timeout = PoolConfig::default().transaction_timeout;
    let status = fetched.status.as_db_str().to_string();
    let period_end = fetched.current_period_end;
    let cancel_at_end = fetched.cancel_at_period_end;
    let plan_id = plan.id;
    let plan_code = plan.code.clone();
    let reason = event_id.to_string();
    let customer_id_owned = customer_id.to_string();
    let provider_name = provider.provider_name().to_string();
    let provider_subscription_id_owned = provider_subscription_id.to_string();
    let created_id = tx::with_tx(&state.pool, timeout, |conn| {
        let status = status.clone();
        let reason = reason.clone();
        let plan_code = plan_code.clone();
        let customer_id_owned = customer_id_owned.clone();
        let provider_name = provider_name.clone();
        let provider_subscription_id_owned = provider_subscription_id_owned.clone();
        Box::pin(async move {
            let row = subscriptions::create(
                &mut *conn,
                user_id,
                plan_id,
                &provider_name,
                &customer_id_owned,
                &provider_subscription_id_owned,
                &status,
                period_end,
                cancel_at_end,
            )
            .await
            .map_err(CreateTxError::Db)?;
            audit_logs::insert(
                &mut *conn,
                &NewAuditLog {
                    actor_user_id: None,
                    actor_client: Some("webhook".to_string()),
                    action: SUBSCRIPTION_CREATED_ACTION.to_string(),
                    target_type: "subscription".to_string(),
                    target_id: Some(row.id.to_string()),
                    before: None,
                    after: Some(serde_json::json!({
                        "status": status,
                        "plan_code": plan_code,
                    })),
                    reason: Some(reason),
                },
            )
            .await
            .map_err(CreateTxError::Db)?;
            Ok(row.id)
        })
    })
    .await
    .map_err(|err| match err {
        TxError::Inner(CreateTxError::Db(db_err)) => {
            tracing::error!(error = ?db_err, "webhook handler: create transaction failed");
            AppError::Internal
        }
        other => {
            tracing::error!(error = ?other, "webhook handler: create transaction failed");
            AppError::Internal
        }
    })?;
    // The new row changes what `effective_plan` resolves, so
    // the catalog flushes after commit (the P1-064 rule).
    state.plans.invalidate();
    Ok(HandlerOutcome::Created {
        subscription_id: created_id,
    })
}
