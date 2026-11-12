//! Detag billing: BillingProvider trait + Stripe adapter + Razorpay adapter (flagged).

pub mod provider;
pub mod razorpay;
pub mod stripe;

#[cfg(test)]
mod tests {
    #[test]
    fn crate_loads() {
        assert!(!env!("CARGO_PKG_NAME").is_empty());
    }
}
