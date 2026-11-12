//! Prometheus metrics (P0-026, FR-X-02).
//!
//! One [`Metrics`] per process: a **custom** [`Registry`] (never the
//! global default — two registries in one process, e.g. in tests, must
//! stay independent) holding the process collector (CPU, memory, fds,
//! …) plus the custom `detag_build_info` gauge (`version` label).
//! Custom application metrics register into the same registry:
//! HTTP request metrics ([`HttpMetrics`], P0-028), queue gauges
//! (P0-029).
//!
//! The `/metrics` route itself lives in the api crate
//! (`apps/api/src/metrics.rs`): it serves [`Metrics::gather`] on a
//! separate internal port (never on the public api port), bound to
//! loopback. Compose network isolation (internal network only, port
//! not published) lands with E0.9 — see ASSUMPTION-003 in
//! `docs/09-STATE/ISSUES.md`.

use prometheus::{
    Encoder, HistogramOpts, HistogramVec, IntCounterVec, IntGauge, IntGaugeVec, Opts, Registry,
    TextEncoder,
};
use std::time::Duration;

/// The process's metrics registry.
#[derive(Clone, Debug)]
pub struct Metrics {
    registry: Registry,
    http: HttpMetrics,
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
        let http = HttpMetrics::new(&registry);
        Metrics { registry, http }
    }

    /// The registry (for registering custom metrics later, P0-028/029).
    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// The HTTP request metrics (counter, duration histogram, in-flight
    /// gauge) registered in this registry.
    pub fn http(&self) -> &HttpMetrics {
        &self.http
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

/// HTTP request metrics (P0-028): total requests, duration histogram,
/// and in-flight gauge — all labelled by route TEMPLATE (+ method, +
/// status where known). Raw ids never appear in labels (cardinality);
/// unmatched paths report as `unmatched` (same convention as the
/// request-id span, P0-019).
#[derive(Clone, Debug)]
pub struct HttpMetrics {
    requests_total: IntCounterVec,
    duration_seconds: HistogramVec,
    inflight: IntGaugeVec,
}

impl HttpMetrics {
    /// Register the three families into `registry`.
    pub fn new(registry: &Registry) -> Self {
        let requests_total = IntCounterVec::new(
            Opts::new(
                "detag_http_requests_total",
                "HTTP requests served, by route template, method and status.",
            ),
            &["route", "method", "status"],
        )
        .expect("detag_http_requests_total builds");
        let duration_seconds = HistogramVec::new(
            HistogramOpts::new(
                "detag_http_request_duration_seconds",
                "HTTP request duration in seconds, by route template, method and status.",
            )
            .buckets(vec![
                0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
            ]),
            &["route", "method", "status"],
        )
        .expect("detag_http_request_duration_seconds builds");
        let inflight = IntGaugeVec::new(
            Opts::new(
                "detag_http_requests_inflight",
                "HTTP requests currently in flight, by route template and method.",
            ),
            &["route", "method"],
        )
        .expect("detag_http_requests_inflight builds");
        registry
            .register(Box::new(requests_total.clone()))
            .expect("requests_total registers into a fresh registry");
        registry
            .register(Box::new(duration_seconds.clone()))
            .expect("duration_seconds registers into a fresh registry");
        registry
            .register(Box::new(inflight.clone()))
            .expect("inflight registers into a fresh registry");
        HttpMetrics {
            requests_total,
            duration_seconds,
            inflight,
        }
    }

    /// Mark one request started; the returned guard decrements the
    /// in-flight gauge when dropped (end of the request).
    pub fn start(&self, route: &str, method: &str) -> InflightGuard<'_> {
        self.inflight.with_label_values(&[route, method]).inc();
        InflightGuard {
            metrics: self,
            route: route.to_string(),
            method: method.to_string(),
        }
    }

    /// Record a finished request (counter + histogram).
    pub fn observe(&self, route: &str, method: &str, status: u16, duration: Duration) {
        let status = status.to_string();
        self.requests_total
            .with_label_values(&[route, method, &status])
            .inc();
        self.duration_seconds
            .with_label_values(&[route, method, &status])
            .observe(duration.as_secs_f64());
    }
}

/// RAII guard: decrements the in-flight gauge on drop.
pub struct InflightGuard<'a> {
    metrics: &'a HttpMetrics,
    route: String,
    method: String,
}

impl Drop for InflightGuard<'_> {
    fn drop(&mut self) {
        self.metrics
            .inflight
            .with_label_values(&[&self.route, &self.method])
            .dec();
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

    #[test]
    fn http_metrics_record_and_gather() {
        let metrics = Metrics::new();
        let http = metrics.http();
        // In-flight is 1 while the guard lives, 0 after it drops.
        let guard = http.start("/healthz", "GET");
        assert!(metrics
            .gather()
            .contains("detag_http_requests_inflight{method=\"GET\",route=\"/healthz\"} 1"));
        drop(guard);
        assert!(metrics
            .gather()
            .contains("detag_http_requests_inflight{method=\"GET\",route=\"/healthz\"} 0"));
        http.observe("/healthz", "GET", 200, Duration::from_millis(5));
        http.observe("/healthz", "GET", 200, Duration::from_millis(15));
        http.observe("/users/{id}", "GET", 404, Duration::from_millis(2));
        let text = metrics.gather();
        assert!(
            text.contains(
                "detag_http_requests_total{method=\"GET\",route=\"/healthz\",status=\"200\"} 2"
            ),
            "counter missing:\n{text}"
        );
        assert!(
            text.contains(
                "detag_http_request_duration_seconds_count{method=\"GET\",route=\"/healthz\",status=\"200\"} 2"
            ),
            "histogram count missing:\n{text}"
        );
        assert!(
            text.contains("detag_http_request_duration_seconds_sum{"),
            "histogram sum missing:\n{text}"
        );
        assert!(
            text.contains(
                "detag_http_requests_total{method=\"GET\",route=\"/users/{id}\",status=\"404\"} 1"
            ),
            "template-labelled 404 missing:\n{text}"
        );
    }
}
