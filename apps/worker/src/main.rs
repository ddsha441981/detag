//! detag-worker — Apalis worker host (bootstrap stub, P0-002).
//! Queue wiring lands in E0.8 (P0-075..P0-082).

use detag_core::config::{warn_unknown_feature_flags, Settings};
use detag_core::shutdown::shutdown_signal;

#[tokio::main]
async fn main() {
    // Fail boot fast on invalid configuration, naming the bad PF_* var.
    let _settings = Settings::load().unwrap_or_else(|err| {
        eprintln!("detag-worker: invalid configuration:\n{err}");
        std::process::exit(1);
    });
    // Unknown PF_FEATURE_* flags warn (a typo must not silently do nothing).
    warn_unknown_feature_flags();
    println!("detag-worker started (queues: pipeline, cleanup, email, reconciliation)");
    // Graceful shutdown (P0-023): SIGTERM/SIGINT stops picking up new
    // jobs; the in-flight job finishes before exit. No pools exist yet
    // (E0.4/E0.8) — their close calls hook in here.
    shutdown_signal().await;
    println!("detag-worker shutting down");
}

#[cfg(test)]
mod tests {
    #[test]
    fn worker_crate_loads() {
        assert!(!env!("CARGO_PKG_NAME").is_empty());
    }
}
