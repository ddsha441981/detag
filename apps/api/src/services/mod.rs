//! Services (Phase 1). The first real handler's home for its
//! business logic (adding-an-endpoint checklist): handlers stay
//! thin (extract → call a service → render), SQL stays in
//! `detag_core::repos`, and the service owns the transaction.

pub mod admin;
pub mod admin_plans;
pub mod api_keys;
pub mod catalog_sync;
pub mod checkout;
pub mod deletion;
pub mod entitlements;
pub mod login;
pub mod logout;
pub mod password;
pub mod password_reset;
pub mod profile;
pub mod pwned;
pub mod quota;
pub mod refresh;
pub mod register;
pub mod revocation;
pub mod sessions;
pub mod usage_attribution;
pub mod verification;
