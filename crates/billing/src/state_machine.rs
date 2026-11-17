//! detag-billing::state_machine — the subscription state machine
//! (P1-064, FR-P-04).
//!
//! The reusable transition layer P1-065's webhook handlers call:
//! pure rules here, persistence + side effects in
//! `apps/api/src/services/subscription_state.rs`.
//!
//! States are exactly the four values the `subscriptions.status`
//! CHECK admits (`trialing` / `active` / `past_due` / `canceled` —
//! [`SubscriptionStatus`], P1-059). The rules:
//!
//! * `trialing → active | past_due | canceled` — the trial
//!   converts, the first payment fails, or the trial is ended.
//! * `active → past_due | canceled` — a renewal payment fails,
//!   or the subscription is ended.
//! * `past_due → active | canceled` — the payment recovers, or
//!   the subscription is ended. Recovery is what makes `past_due`
//!   a state rather than a sentence: P1-049 grants nothing while
//!   past due, and grants again the moment it returns to `active`.
//! * `canceled` is terminal — any move out of it is rejected.
//!   Rows are never deleted, so a canceled row stays as history.
//! * No move ever returns to `trialing` — a trial happens once.
//! * A move to the state the row already holds is a **no-op**,
//!   not an error (`changed: false`, no writes). Webhooks
//!   redeliver and arrive out of order (P1-066): replaying the
//!   event that already applied must be a single effect, and a
//!   stale event restating the current state must not fail the
//!   handler — the state machine is the guard P1-066 leans on.
//!
//! [`target_for_event`] maps a [`DomainEvent`] to the status its
//! handler should move the row to, or `None` when the event
//! carries no state change: `CheckoutCompleted` creates the row
//! (P1-065 inserts it at the provider-reported status —
//! creation is not a transition), and `InvoicePaid` changes
//! nothing by itself (a past-due recovery arrives as a
//! `SubscriptionStatusChanged` to `active` alongside it).

use crate::error::BillingError;
use crate::provider::{DomainEvent, SubscriptionStatus};

/// The outcome of asking the machine for a move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transition {
    /// The state the row was in.
    pub from: SubscriptionStatus,
    /// The state the row moves to (`from` when `changed` is false).
    pub to: SubscriptionStatus,
    /// Whether the row actually changes. A same-state ask is a
    /// no-op: the caller writes nothing, audits nothing, mails
    /// nothing — a replayed event has a single effect.
    pub changed: bool,
}

/// Validate a move from `from` to `to`.
///
/// Same-state is Ok with `changed: false`; every other illegal
/// move (out of `canceled`, back to `trialing`, `active →
/// trialing`, `past_due → trialing`) is
/// [`BillingError::InvalidTransition`].
pub fn transition(
    from: SubscriptionStatus,
    to: SubscriptionStatus,
) -> Result<Transition, BillingError> {
    if from == to {
        return Ok(Transition {
            from,
            to,
            changed: false,
        });
    }
    let allowed = matches!(
        (from, to),
        (SubscriptionStatus::Trialing, SubscriptionStatus::Active)
            | (SubscriptionStatus::Trialing, SubscriptionStatus::PastDue)
            | (SubscriptionStatus::Trialing, SubscriptionStatus::Canceled)
            | (SubscriptionStatus::Active, SubscriptionStatus::PastDue)
            | (SubscriptionStatus::Active, SubscriptionStatus::Canceled)
            | (SubscriptionStatus::PastDue, SubscriptionStatus::Active)
            | (SubscriptionStatus::PastDue, SubscriptionStatus::Canceled)
    );
    if allowed {
        Ok(Transition {
            from,
            to,
            changed: true,
        })
    } else {
        Err(BillingError::InvalidTransition(format!(
            "{} -> {}",
            from.as_db_str(),
            to.as_db_str()
        )))
    }
}

/// The status a [`DomainEvent`] moves a subscription row to, or
/// `None` when the event carries no state change (see the module
/// docs). P1-065 calls this before
/// [`transition`]: `None` means "no row write", `Some(to)` means
/// "validate the move from the row's current status".
pub fn target_for_event(event: &DomainEvent) -> Option<SubscriptionStatus> {
    match event {
        DomainEvent::SubscriptionStatusChanged { status, .. } => Some(*status),
        DomainEvent::SubscriptionDeleted { .. } => Some(SubscriptionStatus::Canceled),
        DomainEvent::CheckoutCompleted { .. } | DomainEvent::InvoicePaid { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::SubscriptionStatus::*;

    /// Every (from, to) pair, and what the machine says about it.
    /// The AC is the invalid half: each `false` below must be an
    /// [`BillingError::InvalidTransition`], never a silent no-op
    /// and never a coercion.
    #[test]
    fn transition_matrix() {
        // (from, to, allowed)
        let cases = [
            (Trialing, Trialing, true),
            (Trialing, Active, true),
            (Trialing, PastDue, true),
            (Trialing, Canceled, true),
            (Active, Trialing, false),
            (Active, Active, true),
            (Active, PastDue, true),
            (Active, Canceled, true),
            (PastDue, Trialing, false),
            (PastDue, Active, true),
            (PastDue, PastDue, true),
            (PastDue, Canceled, true),
            (Canceled, Trialing, false),
            (Canceled, Active, false),
            (Canceled, PastDue, false),
            (Canceled, Canceled, true),
        ];
        for (from, to, allowed) in cases {
            let got = transition(from, to);
            if !allowed {
                assert!(
                    matches!(got, Err(BillingError::InvalidTransition(_))),
                    "{from:?} -> {to:?} must be rejected, got {got:?}"
                );
            } else if from == to {
                assert_eq!(
                    got,
                    Ok(Transition {
                        from,
                        to,
                        changed: false
                    }),
                    "{from:?} -> {to:?} must be a no-op"
                );
            } else {
                assert_eq!(
                    got,
                    Ok(Transition {
                        from,
                        to,
                        changed: true
                    }),
                    "{from:?} -> {to:?} must be allowed"
                );
            }
        }
    }

    #[test]
    fn invalid_transition_names_both_states() {
        let err = transition(Canceled, Active).unwrap_err();
        assert_eq!(
            err.to_string(),
            "invalid subscription transition: canceled -> active"
        );
    }

    #[test]
    fn canceled_is_terminal() {
        for to in [Trialing, Active, PastDue] {
            assert!(
                transition(Canceled, to).is_err(),
                "canceled -> {to:?} must be rejected"
            );
        }
    }

    #[test]
    fn trial_never_repeats() {
        for from in [Active, PastDue] {
            assert!(
                transition(from, Trialing).is_err(),
                "{from:?} -> trialing must be rejected"
            );
        }
    }

    #[test]
    fn event_targets() {
        assert_eq!(
            target_for_event(&DomainEvent::SubscriptionStatusChanged {
                subscription_id: "sub_1".to_string(),
                status: PastDue,
                current_period_end: None,
                cancel_at_period_end: false,
            }),
            Some(PastDue)
        );
        assert_eq!(
            target_for_event(&DomainEvent::SubscriptionDeleted {
                subscription_id: "sub_1".to_string(),
            }),
            Some(Canceled)
        );
        assert_eq!(
            target_for_event(&DomainEvent::CheckoutCompleted {
                customer_id: "cus_1".to_string(),
                subscription_id: "sub_1".to_string(),
            }),
            None,
            "checkout creates the row — it is not a transition"
        );
        assert_eq!(
            target_for_event(&DomainEvent::InvoicePaid {
                invoice_id: "in_1".to_string(),
                subscription_id: Some("sub_1".to_string()),
            }),
            None,
            "an invoice payment alone moves no state"
        );
    }
}
