//! Detag billing: `BillingProvider` trait + Stripe adapter + Razorpay adapter (flagged).
//!
//! The trait ([`provider::BillingProvider`], P1-059, FR-P-05) is the
//! contract the api programs against — provider-agnostic domain types
//! only, no Stripe or Razorpay type crosses it. The Stripe adapter
//! landed in P1-060; the Razorpay adapter landed in P1-073 behind
//! its flag (`PF_FEATURE_RAZORPAY`, off by default). Neither touches
//! core code: the api holds a [`provider::DynBillingProvider`],
//! core does not depend on this crate at all.

pub mod error;
pub mod proration;
pub mod provider;
pub mod razorpay;
pub mod state_machine;
pub mod stripe;
pub mod sync;

pub use error::BillingError;
pub use proration::{
    classify, period_start_from_end, preview, preview_no_period, ChangeKind, ProrationPreview,
};
pub use provider::{
    BillingEvent, BillingProvider, CheckoutRequest, CheckoutSession, Customer, DomainEvent,
    DynBillingProvider, Invoice, PortalRequest, PortalSession, ProviderName, SubscriptionState,
    SubscriptionStatus, PROVIDER_RAZORPAY, PROVIDER_STRIPE,
};
pub use razorpay::RazorpayAdapter;
pub use state_machine::{target_for_event, transition, Transition};
pub use stripe::StripeAdapter;
pub use sync::{
    sync_catalog, CatalogDriftReport, CatalogPlan, CatalogProvider, PlanSyncResult, ProviderPrice,
    ProviderProduct, SyncAction,
};

#[cfg(test)]
mod tests {
    #[test]
    fn crate_loads() {
        assert!(!env!("CARGO_PKG_NAME").is_empty());
    }
}
