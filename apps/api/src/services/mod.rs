//! Services (Phase 1). The first real handler's home for its
//! business logic (adding-an-endpoint checklist): handlers stay
//! thin (extract → call a service → render), SQL stays in
//! `detag_core::repos`, and the service owns the transaction.

pub mod register;
