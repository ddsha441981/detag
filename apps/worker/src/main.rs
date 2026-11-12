//! detag-worker — Apalis worker host (bootstrap stub, P0-002).
//! Queue wiring lands in E0.8 (P0-075..P0-082).

use detag_core::config::{warn_unknown_feature_flags, Settings};
use detag_core::metrics::Metrics;
use detag_core::shutdown::shutdown_signal;
use detag_core::telemetry::{init_tracing, shutdown_telemetry};
use queue_hooks::{QueueHooks, QUEUES};

mod queue_hooks;

#[tokio::main]
async fn main() {
    // Fail boot fast on invalid configuration, naming the bad PF_* var.
    let settings = Settings::load().unwrap_or_else(|err| {
        eprintln!("detag-worker: invalid configuration:\n{err}");
        std::process::exit(1);
    });
    // Tracing first, so the warn below (and everything after) is logged.
    init_tracing(&settings, "detag-worker");
    // Unknown PF_FEATURE_* flags warn (a typo must not silently do nothing).
    warn_unknown_feature_flags();
    // Job/queue metrics hooks (P0-029): the registry is owned by this
    // process; the hooks are the queue implementation's recording
    // surface (E0.8) and the pipeline stages' timers (Phases 2–3).
    // Today only the zero depths are initialised — no values are
    // faked before a real queue/stage exists.
    let metrics = Metrics::new();
    let hooks = QueueHooks::new(metrics.queue().clone());
    hooks.reset_depths();
    tracing::info!(
        queues = QUEUES.join(","),
        "started (queues: pipeline, cleanup, email, reconciliation)"
    );
    // Graceful shutdown (P0-023): SIGTERM/SIGINT stops picking up new
    // jobs; the in-flight job finishes before exit. No pools exist yet
    // (E0.4/E0.8) — their close calls hook in here.
    shutdown_signal().await;
    shutdown_telemetry();
    tracing::info!("shutting down");
}

#[cfg(test)]
mod tests {
    #[test]
    fn worker_crate_loads() {
        assert!(!env!("CARGO_PKG_NAME").is_empty());
    }
}
