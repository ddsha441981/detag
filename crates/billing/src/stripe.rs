//! detag-billing::stripe — the Stripe adapter; lands in P1-060.
//!
//! It will implement [`crate::provider::BillingProvider`] over
//! `async-stripe` (test mode enforced — live keys rejected without an
//! owner flag). The trait it implements already exists
//! (`provider.rs`, P1-059).
