//! Services (Phase 1). The first real handler's home for its
//! business logic (adding-an-endpoint checklist): handlers stay
//! thin (extract → call a service → render), SQL stays in
//! `detag_core::repos`, and the service owns the transaction.

pub mod abuse_intake;
pub mod admin;
pub mod admin_abuse_queue;
pub mod admin_attestations;
pub mod admin_audit;
pub mod admin_plans;
pub mod admin_usage;
pub mod admin_user_view;
pub mod api_keys;
pub mod catalog_sync;
pub mod checkout;
pub mod consent;
pub mod deletion;
pub mod dunning;
pub mod email_prefs;
pub mod enforcement;
pub mod entitlements;
pub mod invoices;
pub mod login;
pub mod logout;
pub mod onboarding;
pub mod password;
pub mod password_reset;
pub mod plan_changes;
pub mod portal;
pub mod profile;
pub mod pwned;
pub mod quota;
pub mod quota_grant;
pub mod quota_warnings;
pub mod refresh;
pub mod refund_tool;
pub mod register;
pub mod revocation;
pub mod sessions;
pub mod subscription_state;
pub mod support_cancel;
pub mod ticket_notes;
pub mod usage;
pub mod usage_attribution;
pub mod user_lookup;
pub mod verification;
pub mod webhook;
pub mod webhook_handlers;
pub mod weekly_digest;
