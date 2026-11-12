//! Seed CLI (P0-043). Thin wrapper over `detag_core::seed`;
//! `just seed` drives it.
//!
//!   cargo run -p detag-core --example seed
//!
//! Idempotent: run twice, the second run changes nothing (every
//! INSERT is ON CONFLICT DO NOTHING). Errors are printed redacted
//! (P0-032: never log credentials).

use detag_core::config::Settings;
use detag_core::db::{connect, PoolConfig};
use detag_core::seed::seed;
use detag_core::telemetry::redact;

#[tokio::main]
async fn main() {
    let settings = Settings::load().unwrap_or_else(|err| {
        eprintln!("seed: invalid configuration:\n{err}");
        std::process::exit(1);
    });
    let pool = connect(&settings.database_url, &PoolConfig::default())
        .await
        .unwrap_or_else(|err| {
            eprintln!("seed: {}", redact(&err.to_string()));
            std::process::exit(1);
        });
    match seed(&pool).await {
        Ok(report) => {
            println!(
                "seeded: {} plans, {} feature flags (idempotent — re-run changes nothing)",
                report.plans, report.flags
            );
        }
        Err(err) => {
            eprintln!("seed: {}", redact(&err.to_string()));
            std::process::exit(1);
        }
    }
    pool.close().await;
}
