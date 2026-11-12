//! Graceful shutdown (P0-023): wait for SIGTERM or SIGINT, then drain.
//!
//! Both binaries use the same signal: the api stops accepting new
//! connections and drains in-flight requests (axum's
//! `with_graceful_shutdown`), the worker stops picking up new jobs
//! and finishes the in-flight one. How long a drain may take is
//! bounded by [`SHUTDOWN_TIMEOUT`] — past it, the process exits
//! non-zero rather than hanging forever (a stuck connection must not
//! block a deploy).

use std::time::Duration;

/// How long a drain may take before the process forces its exit.
pub const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30);

/// Resolve when the process should shut down: the first SIGTERM or
/// SIGINT. SIGTERM is what orchestrators send (`docker stop`,
/// Kubernetes); SIGINT is Ctrl-C in a terminal.
#[cfg(unix)]
pub async fn shutdown_signal() {
    use tokio::signal::unix::{signal, SignalKind};
    let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler installs");
    let mut interrupt = signal(SignalKind::interrupt()).expect("SIGINT handler installs");
    tokio::select! {
        _ = term.recv() => {},
        _ = interrupt.recv() => {},
    }
}

/// Non-Unix fallback: only Ctrl-C exists.
#[cfg(not(unix))]
pub async fn shutdown_signal() {
    tokio::signal::ctrl_c()
        .await
        .expect("Ctrl-C handler installs");
}
