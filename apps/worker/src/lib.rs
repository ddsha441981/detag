//! detag-worker — Apalis worker host library (P0-075).
//!
//! The binary (`main.rs`) only loads settings, builds the
//! monitor, and runs it until SIGTERM. Everything testable
//! lives here so integration tests (`apps/worker/tests/`)
//! can drive it without spawning the binary.

pub mod envelope;
pub mod queue_hooks;
pub mod registry;
pub mod workers;
