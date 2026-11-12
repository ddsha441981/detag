//! Job/queue metrics hooks (P0-029, FR-X-02).
//!
//! The worker skeleton owns one [`QueueHooks`] for the process
//! lifetime. The hooks are the only way queue/stage code records
//! metrics: [`QueueHooks::enqueued`] / [`QueueHooks::dequeued`] around
//! queue operations, [`QueueHooks::set_depth`] when depth changes, and
//! [`QueueHooks::stage_finished`] at the end of each pipeline stage.
//! The real calls land with the queue implementation (E0.8, P0-075+)
//! and the pipeline stages (Phases 2–3); until then the worker only
//! initialises all depths to 0 at startup ([`QueueHooks::reset_depths`])
//! so the series exist with honest zero values.
//!
//! Label vocabulary lives here, next to the callers: `QUEUES` are the
//! four worker queues. The stage vocabulary (`intake`, `detect`,
//! `mask`, `inpaint`, `encode`, `qc`) is documented on
//! [`QueueMetrics`](detag_core::metrics::QueueMetrics); a `STAGES`
//! constant lands with the first pipeline stage that calls
//! [`QueueHooks::stage_finished`].

use detag_core::metrics::QueueMetrics;
use std::time::Duration;

/// The four worker queues (see `apps/worker/src/main.rs`).
pub const QUEUES: [&str; 4] = ["pipeline", "cleanup", "email", "reconciliation"];

/// The worker's handle to the job/queue metrics. Cheap to clone: the
/// underlying counters live in the shared [`QueueMetrics`] registry.
#[derive(Clone, Debug)]
pub struct QueueHooks {
    metrics: QueueMetrics,
}

// NOTE (P0-029): `enqueued` / `dequeued` / `set_depth` /
// `stage_finished` are intentionally unused until the queue
// implementation (E0.8, P0-075+) and the pipeline stages (Phases 2–3)
// call them — that is the point of this hooks module. The `allow` is
// scoped to this impl and removed by the ticket that first calls the
// hooks; `reset_depths` (called from `main`) is the only live call
// today, so no values are faked before a real queue/stage exists.
#[allow(dead_code)]
impl QueueHooks {
    /// Wrap the queue metrics of this process's registry.
    pub fn new(metrics: QueueMetrics) -> Self {
        QueueHooks { metrics }
    }

    /// Set every queue's depth to 0 (worker startup: no jobs yet).
    pub fn reset_depths(&self) {
        for queue in QUEUES {
            self.metrics.set_depth(queue, 0);
        }
    }

    /// Record one job enqueued on `queue`.
    pub fn enqueued(&self, queue: &str) {
        self.metrics.enqueue(queue);
    }

    /// Record one job dequeued (picked up by a worker) from `queue`.
    pub fn dequeued(&self, queue: &str) {
        self.metrics.dequeue(queue);
    }

    /// Set the current depth of `queue` (jobs waiting, not yet picked
    /// up).
    pub fn set_depth(&self, queue: &str, depth: i64) {
        self.metrics.set_depth(queue, depth);
    }

    /// Record how long one run of pipeline `stage` took.
    pub fn stage_finished(&self, stage: &str, duration: Duration) {
        self.metrics.observe_stage(stage, duration);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use detag_core::metrics::Metrics;

    #[test]
    fn hooks_record_through_to_gather() {
        let metrics = Metrics::new();
        let hooks = QueueHooks::new(metrics.queue().clone());
        hooks.reset_depths();
        hooks.enqueued("pipeline");
        hooks.dequeued("pipeline");
        hooks.set_depth("cleanup", 2);
        hooks.stage_finished("detect", Duration::from_millis(120));
        let text = metrics.gather();
        assert!(
            text.contains("detag_queue_depth{queue=\"pipeline\"} 0"),
            "reset depth missing:\n{text}"
        );
        assert!(
            text.contains("detag_queue_depth{queue=\"cleanup\"} 2"),
            "set depth missing:\n{text}"
        );
        assert!(
            text.contains("detag_queue_enqueued_total{queue=\"pipeline\"} 1"),
            "enqueue hook missing:\n{text}"
        );
        assert!(
            text.contains("detag_queue_dequeued_total{queue=\"pipeline\"} 1"),
            "dequeue hook missing:\n{text}"
        );
        assert!(
            text.contains("detag_job_stage_duration_seconds_count{stage=\"detect\"} 1"),
            "stage hook missing:\n{text}"
        );
    }

    #[test]
    fn vocabulary_covers_worker_queues() {
        // The constant is the contract with the queue implementation:
        // if a queue is added, it lands here.
        assert_eq!(QUEUES.len(), 4);
        assert!(QUEUES.contains(&"pipeline"));
        assert!(QUEUES.contains(&"reconciliation"));
    }
}
