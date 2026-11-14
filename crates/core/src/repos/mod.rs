//! SQL repositories (P0-041, E0.4).
//!
//! One module per aggregate (`users`, `jobs`, `plans`); SQL lives
//! here, not in services (coding standards). Queries use sqlx
//! `query_as!` macros — compile-time checked against the live
//! schema, with offline data checked in under `.sqlx/` (run
//! `just sqlx-prepare` after changing a query; CI runs
//! `just sqlx-prepare-check`).
//!
//! Error mapping: Postgres constraint failures become typed
//! `RepoError` variants (`error.rs`) instead of raw `sqlx::Error`.
//!
//! Every function takes `impl Executor` rather than `&DbPool`, so
//! the same call works against the pool and inside a transaction
//! (`tx::with_tx` hands the closure a `&mut PgConnection`, which is
//! an executor). Pass `&pool` at the top level, `&mut *conn` inside
//! `with_tx` (a reborrow — the closure owns the `&mut`).
//!
//! Testcontainers is the kit's intended fixture for these tests,
//! but docker cannot start containers on this VM (DOCKER-001), so
//! the tests run against the native postgres with a scratch
//! database — the same pattern as the migration live tests. The
//! Testcontainers form belongs with the fixtures work (P0-092).

pub mod api_keys;
pub mod artifacts;
pub mod audit_logs;
pub mod email_outbox;
pub mod email_verification_tokens;
pub mod error;
pub mod job_events;
pub mod jobs;
pub mod login_attempts;
pub mod plans;
pub mod sessions;
pub mod subscriptions;
pub mod uploads;
pub mod usage_counters;
pub mod users;
pub mod worker_heartbeats;

pub use error::RepoError;
