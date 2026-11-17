//! Detag billing: `BillingProvider` trait + Stripe adapter + Razorpay adapter (flagged).
//!
//! The trait ([`provider::BillingProvider`], P1-059, FR-P-05) is the
//! contract the api programs against — provider-agnostic domain types
//! only, no Stripe or Razorpay type crosses it. The Stripe adapter
//! lands in P1-060; the Razorpay adapter lands behind its flag after
//! it. Neither touches core code: the api holds a
//! [`provider::DynBillingProvider`], core does not depend on this crate
//! at all.

pub mod error;
pub mod provider;
pub mod razorpay;
pub mod stripe;
pub mod sync;

pub use error::BillingError;
pub use provider::{
    BillingEvent, BillingProvider, CheckoutRequest, CheckoutSession, Customer, DomainEvent,
    DynBillingProvider, Invoice, PortalRequest, PortalSession, ProviderName, SubscriptionState,
    SubscriptionStatus, PROVIDER_RAZORPAY, PROVIDER_STRIPE,
};
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
