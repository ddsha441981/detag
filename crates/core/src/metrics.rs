//! Prometheus metrics (P0-026, FR-X-02).
//!
//! One [`Metrics`] per process: a **custom** [`Registry`] (never the
//! global default — two registries in one process, e.g. in tests, must
//! stay independent) holding the process collector (CPU, memory, fds,
//! …) plus the custom `detag_build_info` gauge (`version` label).
//! Custom application metrics register into the same registry:
//! HTTP request metrics ([`HttpMetrics`], P0-028), job/queue
//! metrics ([`QueueMetrics`], P0-029).
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
    queue: QueueMetrics,
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
        let queue = QueueMetrics::new(&registry);
        Metrics {
            registry,
            http,
            queue,
        }
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

    /// The job/queue metrics (depth gauge, enqueue/dequeue counters,
    /// stage timers) registered in this registry.
    pub fn queue(&self) -> &QueueMetrics {
        &self.queue
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

/// Job/queue metrics (P0-029, FR-X-02): queue depth gauge, enqueue /
/// dequeue counters, and per-stage duration histograms. Registered
/// from the worker skeleton; the values are wired in by the queue
/// implementation (E0.8) and the pipeline stages (Phases 2–3), so
/// today these series are zero-valued until a job flows through.
///
/// Label vocabulary (fixed, low cardinality — never a job id):
/// * `queue`: one of `pipeline`, `cleanup`, `email`,
///   `reconciliation` (the four worker queues, see
///   `apps/worker/src/queue_hooks.rs`).
/// * `stage`: one of `intake`, `detect`, `mask`, `inpaint`, `encode`,
///   `qc` (the pipeline stages in `03-ARCHITECTURE/01-system-architecture.md`).
#[derive(Clone, Debug)]
pub struct QueueMetrics {
    depth: IntGaugeVec,
    enqueued_total: IntCounterVec,
    dequeued_total: IntCounterVec,
    stage_duration_seconds: HistogramVec,
}

impl QueueMetrics {
    /// Register the four families into `registry`.
    pub fn new(registry: &Registry) -> Self {
        let depth = IntGaugeVec::new(
            Opts::new(
                "detag_queue_depth",
                "Jobs currently waiting in a queue, by queue.",
            ),
            &["queue"],
        )
        .expect("detag_queue_depth builds");
        let enqueued_total = IntCounterVec::new(
            Opts::new("detag_queue_enqueued_total", "Jobs enqueued, by queue."),
            &["queue"],
        )
        .expect("detag_queue_enqueued_total builds");
        let dequeued_total = IntCounterVec::new(
            Opts::new(
                "detag_queue_dequeued_total",
                "Jobs dequeued (picked up by a worker), by queue.",
            ),
            &["queue"],
        )
        .expect("detag_queue_dequeued_total builds");
        let stage_duration_seconds = HistogramVec::new(
            HistogramOpts::new(
                "detag_job_stage_duration_seconds",
                "Job stage duration in seconds, by pipeline stage.",
            )
            // Stages run from milliseconds (mask plan) to minutes
            // (tiled LaMa inpaint of a 4K image), so the buckets span
            // 10 ms .. 5 min.
            .buckets(vec![
                0.01, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0,
            ]),
            &["stage"],
        )
        .expect("detag_job_stage_duration_seconds builds");
        registry
            .register(Box::new(depth.clone()))
            .expect("queue depth registers into a fresh registry");
        registry
            .register(Box::new(enqueued_total.clone()))
            .expect("enqueue counter registers into a fresh registry");
        registry
            .register(Box::new(dequeued_total.clone()))
            .expect("dequeue counter registers into a fresh registry");
        registry
            .register(Box::new(stage_duration_seconds.clone()))
            .expect("stage histogram registers into a fresh registry");
        QueueMetrics {
            depth,
            enqueued_total,
            dequeued_total,
            stage_duration_seconds,
        }
    }

    /// Set the current depth of `queue` (jobs waiting, not yet picked
    /// up). Called by the queue implementation whenever depth changes;
    /// the worker sets all queues to 0 at startup.
    pub fn set_depth(&self, queue: &str, depth: i64) {
        self.depth.with_label_values(&[queue]).set(depth);
    }

    /// Record one job enqueued on `queue`.
    pub fn enqueue(&self, queue: &str) {
        self.enqueued_total.with_label_values(&[queue]).inc();
    }

    /// Record one job dequeued (picked up by a worker) from `queue`.
    pub fn dequeue(&self, queue: &str) {
        self.dequeued_total.with_label_values(&[queue]).inc();
    }

    /// Record how long one run of pipeline `stage` took.
    pub fn observe_stage(&self, stage: &str, duration: Duration) {
        self.stage_duration_seconds
            .with_label_values(&[stage])
            .observe(duration.as_secs_f64());
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
    fn queue_metrics_registered_and_gathered() {
        // AC: gauges/counters registered. Prometheus vecs only emit a
        // family once at least one label combination exists, so the
        // test touches one series per family (exactly what the worker
        // does at startup with reset_depths) and then asserts all
        // four families appear in the scrape text.
        let metrics = Metrics::new();
        let queue = metrics.queue();
        queue.set_depth("pipeline", 0);
        queue.enqueue("pipeline");
        queue.dequeue("pipeline");
        queue.observe_stage("intake", Duration::from_millis(1));
        let text = metrics.gather();
        for family in [
            "detag_queue_depth",
            "detag_queue_enqueued_total",
            "detag_queue_dequeued_total",
            "detag_job_stage_duration_seconds",
        ] {
            assert!(
                text.contains(family),
                "queue metric family {family} missing:\n{text}"
            );
        }
    }

    #[test]
    fn queue_metrics_record_and_gather() {
        let metrics = Metrics::new();
        let queue = metrics.queue();
        queue.set_depth("pipeline", 3);
        queue.enqueue("pipeline");
        queue.enqueue("pipeline");
        queue.dequeue("pipeline");
        queue.observe_stage("inpaint", Duration::from_millis(250));
        queue.observe_stage("inpaint", Duration::from_millis(750));
        let text = metrics.gather();
        assert!(
            text.contains("detag_queue_depth{queue=\"pipeline\"} 3"),
            "depth gauge missing:\n{text}"
        );
        assert!(
            text.contains("detag_queue_enqueued_total{queue=\"pipeline\"} 2"),
            "enqueue counter missing:\n{text}"
        );
        assert!(
            text.contains("detag_queue_dequeued_total{queue=\"pipeline\"} 1"),
            "dequeue counter missing:\n{text}"
        );
        assert!(
            text.contains("detag_job_stage_duration_seconds_count{stage=\"inpaint\"} 2"),
            "stage histogram count missing:\n{text}"
        );
        assert!(
            text.contains("detag_job_stage_duration_seconds_sum{stage=\"inpaint\"} 1"),
            "stage histogram sum missing:\n{text}"
        );
    }

    #[test]
    fn queue_metrics_are_independent_across_registries() {
        let first = Metrics::new();
        let second = Metrics::new();
        first.queue().enqueue("pipeline");
        first.queue().set_depth("pipeline", 7);
        let text = second.gather();
        assert!(
            !text.contains("detag_queue_enqueued_total{queue=\"pipeline\"} 1"),
            "queue state leaked across registries:\n{text}"
        );
        assert!(
            !text.contains("detag_queue_depth{queue=\"pipeline\"} 7"),
            "queue depth leaked across registries:\n{text}"
        );
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
