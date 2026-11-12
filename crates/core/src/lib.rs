//! Detag core: config, error taxonomy, domain types, job state machine, audit.
//! Module contents land with their epic tickets (E0.2 config/errors, E0.4 db, …).

pub mod audit;
pub mod checksum;
pub mod clock;
pub mod config;
pub mod db;
pub mod error;
pub mod flags;
pub mod media;
pub mod metrics;
pub mod migrate;
pub mod pagination;
pub mod redis;
pub mod repos;
pub mod seed;
pub mod shutdown;
pub mod state_machine;
pub mod telemetry;
pub mod tx;
pub mod types;

/// Crate version (workspace version).
pub const CRATE_VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crate_loads() {
        assert_eq!(CRATE_VERSION, "0.1.0");
    }
}
