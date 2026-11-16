//! detag-billing::razorpay — the Razorpay adapter; lands behind its
//! feature flag after P1-060.
//!
//! It will implement [`crate::provider::BillingProvider`] with webhook
//! HMAC mapping. The trait it implements already exists
//! (`provider.rs`, P1-059). No Razorpay code is called until the flag
//! exists — this module stays a placeholder on purpose.
