//! Prometheus metrics (P0-026, FR-X-02).
//!
//! One [`Metrics`] per process: a **custom** [`Registry`] (never the
//! global default — two registries in one process, e.g. in tests, must
//! stay independent) holding the process collector (CPU, memory, fds,
//! …) plus the custom `detag_build_info` gauge (`version` label).
//! Custom application metrics (HTTP histograms in P0-028, queue gauges
//! in P0-029) register into the same registry later.
//!
//! The `/metrics` route itself lives in the api crate
//! (`apps/api/src/metrics.rs`): it serves [`Metrics::gather`] on a
//! separate internal port (never on the public api port), bound to
//! loopback. Compose network isolation (internal network only, port
//! not published) lands with E0.9 — see ASSUMPTION-003 in
//! `docs/09-STATE/ISSUES.md`.

use prometheus::{Encoder, IntGauge, Opts, Registry, TextEncoder};

/// The process's metrics registry.
#[derive(Clone, Debug)]
pub struct Metrics {
    registry: Registry,
}

impl Metrics {
    /// A fresh registry with the process collector and `detag_build_info`.
    pub fn new() -> Self {
        let registry =
            Registry::new_custom(None, None).expect("a fresh custom registry always builds");
        let process_collector = prometheus::process_collector::ProcessCollector::for_self();
        registry
            .register(Box::new(process_collector))
            .expect("process collector registers into a fresh registry");
        let build_info = IntGauge::with_opts(
            Opts::new(
                "detag_build_info",
                "Build info: 1 for this binary's version.",
            )
            .const_label("version", env!("CARGO_PKG_VERSION")),
        )
        .expect("detag_build_info gauge builds");
        build_info.set(1);
        registry
            .register(Box::new(build_info))
            .expect("detag_build_info registers into a fresh registry");
        Metrics { registry }
    }

    /// The registry (for registering custom metrics later, P0-028/029).
    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// All metrics in Prometheus text format (what `/metrics` serves).
    pub fn gather(&self) -> String {
        let mut buffer = Vec::new();
        TextEncoder::new()
            .encode(&self.registry.gather(), &mut buffer)
            .expect("gathered metrics always encode");
        String::from_utf8(buffer).expect("Prometheus text is UTF-8")
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gather_contains_process_metrics() {
        let text = Metrics::new().gather();
        // The process collector (Linux): CPU time and resident memory.
        assert!(
            text.contains("process_cpu_seconds_total"),
            "process CPU metric missing:\n{text}"
        );
        assert!(
            text.contains("process_resident_memory_bytes"),
            "process memory metric missing:\n{text}"
        );
    }

    #[test]
    fn gather_contains_build_info() {
        let text = Metrics::new().gather();
        assert!(
            text.contains("detag_build_info{version=\"0.1.0\"} 1"),
            "build_info gauge missing:\n{text}"
        );
    }

    #[test]
    fn registries_are_independent() {
        // Two registries must not share state (the point of a custom
        // registry over the global default): gathering one leaves the
        // other untouched, and both carry their own build_info.
        let first = Metrics::new();
        let second = Metrics::new();
        let _ = first.gather();
        assert!(second.gather().contains("detag_build_info"));
        assert!(first.gather().contains("detag_build_info"));
    }
}
