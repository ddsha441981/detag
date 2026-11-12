//! Retry policy (P0-077, E0.8): per-queue max attempts, jittered
//! exponential backoff, max attempts to dead, retry events in
//! `job_events`.
//!
//! Why a tower layer, not the storage's retry: apalis-redis
//! 0.6.4's ack path requeues a failed job immediately (score
//! `now`, swept every ~30 s) and never checks a max-attempts
//! count — the `TODO: Increase the attempts` comment in its
//! `ack` says so — so the storage alone cannot delay a retry or
//! kill a job. The only dead path in the ack flow is
//! [`Error::Abort`], which moves the job to the dead set. This
//! layer therefore owns the whole retry decision in-process:
//!
//! * on failure it sleeps for the backoff delay, then re-runs
//!   the handler in the same worker task (the attempt counter
//!   lives in the request parts and is incremented per retry);
//! * when the attempts are exhausted it returns
//!   `Error::Abort`, so the ack path dead-letters the job;
//! * every decision is recorded: a `job_events` row (via the
//!   [`RetryEventSink`]), the `detag_queue_retried_total` /
//!   `detag_queue_dead_total` counters, and a structured log line.
//!
//! Trade-offs, stated plainly:
//! * A sleeping retry occupies one worker slot for the whole
//!   backoff (up to ~60 min at the cap). With the default 4
//!   workers per queue the queue keeps moving; the sleep is
//!   the price of a real delay with this apalis version.
//! * The attempt counter is in-process only: a worker restart
//!   mid-retry resets the count for that job (the stored
//!   envelope still says attempt 0). Durable attempt tracking
//!   belongs with the envelope's `attempt` field and lands
//!   with the first producer that rewrites it (Phase 2).
//! * Quarantined jobs (unknown kind / bad version / hash
//!   mismatch, P0-076) never reach this layer — they ack, so
//!   they are never retried.
//!
//! Backoff schedule (the AC): after failed attempt 0 → 1 min,
//! after attempt 1 → 5 min, after attempt 2+ → 60 min (cap),
//! each ±25% jitter. The jitter is deterministic per
//! (job, attempt) — a hash of the job id and the attempt —
//! so equal jobs spread across the window instead of
//! thundering-herding on the same second.

use crate::dlq::DeadLetterSink;
use crate::envelope::JobEnvelope;
use crate::queue_hooks::QueueHooks;
use apalis::prelude::*;
use apalis_redis::RedisContext;
use detag_core::db::DbPool;
use detag_core::repos::job_events::{self, RetryEventRow};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;
use tower::{Layer, Service};
use uuid::Uuid;

/// Per-queue retry counts (Phase-0 defaults — constants until
/// an ops ticket env-ifies them). Pipeline jobs are the most
/// expensive to redo, so they get the same 5 attempts as email;
/// cleanup and reconciliation are cheap idempotent sweeps at 3.
pub const MAX_ATTEMPTS_PIPELINE: usize = 5;
/// See [`MAX_ATTEMPTS_PIPELINE`].
pub const MAX_ATTEMPTS_CLEANUP: usize = 3;
/// See [`MAX_ATTEMPTS_PIPELINE`].
pub const MAX_ATTEMPTS_EMAIL: usize = 5;
/// See [`MAX_ATTEMPTS_PIPELINE`].
pub const MAX_ATTEMPTS_RECONCILIATION: usize = 3;
/// Max attempts for a queue this table does not know.
pub const MAX_ATTEMPTS_DEFAULT: usize = 5;

/// The backoff ladder: 1 min, 5 min, 60 min (cap).
pub const BASE_DELAYS: [Duration; 3] = [
    Duration::from_secs(60),
    Duration::from_secs(5 * 60),
    Duration::from_secs(60 * 60),
];

/// The retry policy for one queue: how many attempts a job gets
/// in total (including the first run) and the backoff ladder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Total attempts allowed, first run included.
    pub max_attempts: usize,
    /// Backoff ladder: index 0 after attempt 0, 1 after attempt
    /// 1, 2 (the cap) after attempt 2 and every later attempt.
    pub base_delays: [Duration; 3],
}

impl RetryPolicy {
    /// The policy for `queue` (per-queue max attempts; the
    /// ladder is the same everywhere).
    pub fn for_queue(queue: &str) -> Self {
        let max_attempts = match queue {
            "pipeline" => MAX_ATTEMPTS_PIPELINE,
            "cleanup" => MAX_ATTEMPTS_CLEANUP,
            "email" => MAX_ATTEMPTS_EMAIL,
            "reconciliation" => MAX_ATTEMPTS_RECONCILIATION,
            _ => MAX_ATTEMPTS_DEFAULT,
        };
        Self {
            max_attempts,
            base_delays: BASE_DELAYS,
        }
    }

    /// True when the job may run again after failed attempt
    /// `attempt` (0-based: attempt 0 is the first run).
    pub fn should_retry(&self, attempt: usize) -> bool {
        attempt + 1 < self.max_attempts
    }

    /// The un-jittered backoff after failed attempt `attempt`:
    /// 1 min, then 5 min, then 60 min forever (the cap).
    pub fn base_delay(&self, attempt: usize) -> Duration {
        self.base_delays[attempt.min(self.base_delays.len() - 1)]
    }

    /// The backoff after failed attempt `attempt` for `job_id`,
    /// jittered ±25% around [`Self::base_delay`]. Deterministic
    /// per (job, attempt): the fraction comes from a hash of the
    /// job id and the attempt, so two jobs failing at once spread
    /// across the window instead of retrying in lockstep.
    pub fn delay_for(&self, attempt: usize, job_id: Uuid) -> Duration {
        let base = self.base_delay(attempt);
        let fraction = jitter_fraction(job_id, attempt);
        // 0.75 .. 1.25 of the base.
        base.mul_f64(0.75 + 0.5 * fraction)
    }
}

/// A deterministic fraction in [0, 1) from (job, attempt):
/// FNV-1a over the job id bytes + the attempt, then the top 53
/// bits as a double. No RNG dependency; same inputs give the
/// same delay, different jobs spread.
fn jitter_fraction(job_id: Uuid, attempt: usize) -> f64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in job_id.as_bytes().iter().chain(attempt.to_le_bytes().iter()) {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    // Top 53 bits → [0, 1).
    let bits = (hash >> 11) as f64;
    bits / ((1u64 << 53) as f64)
}

/// One retry decision, for the event sink and the log line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryEvent {
    /// The Detag job being retried.
    pub job_id: Uuid,
    /// The kind that failed (for the log line, never a label).
    pub kind: String,
    /// The queue the job runs on.
    pub queue: &'static str,
    /// The attempt that just failed (0-based).
    pub attempt: usize,
    /// The backoff delay before the next attempt.
    pub delay: Duration,
    /// The handler error text.
    pub error: String,
}

/// Where retry decisions are recorded. The production sink
/// writes `job_events` (best-effort — it must never fail the
/// job); tests use a recording sink. The trait is generic over
/// the sink type (no `dyn`): `async fn` in a trait is not
/// object-safe.
pub trait RetryEventSink: Clone + Send + Sync + 'static {
    /// Record one retry decision. Implementations must not fail
    /// the job: a sink error is logged by the caller, never
    /// propagated.
    fn record_retry(&self, event: RetryEvent) -> impl Future<Output = ()> + Send;
}

/// The production sink: one `job_events` row per retry (P0-077
/// spec). Best-effort: the insert fails when no jobs row exists
/// for the job id (FK) — that is expected in Phase 0, where no
/// producer creates jobs rows yet — so the error is warned, and
/// the retry goes ahead regardless.
#[derive(Debug, Clone)]
pub struct PostgresRetryEventSink {
    pool: DbPool,
}

impl PostgresRetryEventSink {
    /// A sink writing to `pool`.
    pub fn new(pool: DbPool) -> Self {
        Self { pool }
    }
}

impl RetryEventSink for PostgresRetryEventSink {
    async fn record_retry(&self, event: RetryEvent) {
        let row = RetryEventRow {
            attempt: event.attempt as i32,
            delay_ms: job_events::delay_ms(event.delay),
            error: event.error,
        };
        if let Err(err) = job_events::record_retry_event(&self.pool, event.job_id, &row).await {
            tracing::warn!(
                job_id = %event.job_id,
                kind = %event.kind,
                queue = event.queue,
                error = %err,
                "retry event not recorded in job_events (best-effort)"
            );
        }
    }
}

/// A sink that does nothing — for contexts where no pool exists.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopRetryEventSink;

impl RetryEventSink for NoopRetryEventSink {
    async fn record_retry(&self, _event: RetryEvent) {}
}

/// Tower layer that applies a [`RetryPolicy`] around the handler
/// service. Built per queue in `build_monitor`.
#[derive(Debug, Clone)]
pub struct RetryLayer<K> {
    policy: RetryPolicy,
    sink: K,
    hooks: QueueHooks,
    dlq: Arc<dyn DeadLetterSink + Send + Sync>,
    queue: &'static str,
}

impl<K> RetryLayer<K> {
    /// A layer for `queue` with `policy`, event `sink`, metric
    /// `hooks` and dead-letter `dlq`.
    pub fn new(
        policy: RetryPolicy,
        sink: K,
        hooks: QueueHooks,
        dlq: Arc<dyn DeadLetterSink + Send + Sync>,
        queue: &'static str,
    ) -> Self {
        Self {
            policy,
            sink,
            hooks,
            dlq,
            queue,
        }
    }
}

impl<S, K> Layer<S> for RetryLayer<K>
where
    K: Clone,
{
    type Service = RetryService<S, K>;

    fn layer(&self, inner: S) -> Self::Service {
        RetryService {
            inner,
            policy: self.policy,
            sink: self.sink.clone(),
            hooks: self.hooks.clone(),
            dlq: self.dlq.clone(),
            queue: self.queue,
        }
    }
}

/// The retrying service: calls the inner handler, sleeps for the
/// backoff and re-runs it while attempts remain, and aborts
/// (→ dead set) when they are exhausted. The inner service must
/// be `Clone`: each attempt consumes its own clone of the
/// request, and the service is cloned per attempt the same way
/// tower's own retry does it.
#[derive(Debug, Clone)]
pub struct RetryService<S, K> {
    inner: S,
    policy: RetryPolicy,
    sink: K,
    hooks: QueueHooks,
    dlq: Arc<dyn DeadLetterSink + Send + Sync>,
    queue: &'static str,
}

impl<S, K> Service<Request<JobEnvelope, RedisContext>> for RetryService<S, K>
where
    S: Service<Request<JobEnvelope, RedisContext>, Response = (), Error = Error>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
    K: RetryEventSink,
{
    type Response = ();
    type Error = Error;
    type Future = Pin<Box<dyn Future<Output = Result<(), Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<JobEnvelope, RedisContext>) -> Self::Future {
        let mut inner = self.inner.clone();
        let policy = self.policy;
        let sink = self.sink.clone();
        let hooks = self.hooks.clone();
        let dlq = self.dlq.clone();
        let queue = self.queue;
        Box::pin(async move {
            let job_id = req.args.job_id;
            let kind = req.args.kind.clone();
            let req = req;
            loop {
                let attempt = req.parts.attempt.current();
                let outcome = inner.call(req.clone()).await;
                match outcome {
                    Ok(()) => return Ok(()),
                    Err(err) => {
                        // An abort from the handler itself is
                        // final — never retry it. It still goes to
                        // dead (the ack path), so count it and file
                        // it in the DLQ like any other death.
                        if matches!(err, Error::Abort(_)) {
                            hooks.dead(queue);
                            dlq.quarantine(
                                &req.args,
                                queue,
                                "aborted by handler",
                                Some(&err.to_string()),
                            )
                            .await;
                            return Err(err);
                        }
                        if !policy.should_retry(attempt) {
                            hooks.dead(queue);
                            dlq.quarantine(
                                &req.args,
                                queue,
                                "retries exhausted",
                                Some(&err.to_string()),
                            )
                            .await;
                            tracing::error!(
                                job_id = %job_id,
                                kind = %kind,
                                queue = queue,
                                attempt = attempt,
                                max_attempts = policy.max_attempts,
                                error = %err,
                                "job exhausted retries — going to dead"
                            );
                            return Err(Error::Abort(Arc::new(Box::new(RetryExhausted {
                                job_id,
                                attempts: policy.max_attempts,
                                last_error: err.to_string(),
                            })
                                as BoxDynError)));
                        }
                        let delay = policy.delay_for(attempt, job_id);
                        let event = RetryEvent {
                            job_id,
                            kind: kind.clone(),
                            queue,
                            attempt,
                            delay,
                            error: err.to_string(),
                        };
                        sink.record_retry(event).await;
                        hooks.retried(queue);
                        tracing::warn!(
                            job_id = %job_id,
                            kind = %kind,
                            queue = queue,
                            attempt = attempt,
                            delay_ms = delay.as_millis() as u64,
                            error = %err,
                            "job failed — retrying after backoff"
                        );
                        tokio::time::sleep(delay).await;
                        req.parts.attempt.increment();
                    }
                }
            }
        })
    }
}

/// The error carried into the dead set when a job exhausts its
/// attempts: the job id, how many attempts it got, and the last
/// handler error.
#[derive(Debug, thiserror::Error)]
#[error("job {job_id} exhausted {attempts} attempts (last error: {last_error})")]
pub struct RetryExhausted {
    /// The Detag job that died.
    pub job_id: Uuid,
    /// Attempts it was allowed.
    pub attempts: usize,
    /// The last handler error.
    pub last_error: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dlq::test_support::RecordingDeadLetterSink;
    use crate::envelope::JobEnvelope;
    use detag_core::metrics::Metrics;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    /// A recording sink for tests: keeps every event in order.
    #[derive(Debug, Clone, Default)]
    struct RecordingSink {
        events: Arc<Mutex<Vec<RetryEvent>>>,
    }

    impl RetryEventSink for RecordingSink {
        async fn record_retry(&self, event: RetryEvent) {
            self.events.lock().expect("sink lock").push(event);
        }
    }

    /// A mock inner service: fails `failures` times, then succeeds.
    /// Counts every call.
    #[derive(Debug, Clone)]
    struct FlakyService {
        failures_left: Arc<Mutex<usize>>,
        calls: Arc<Mutex<usize>>,
    }

    impl FlakyService {
        fn new(failures: usize) -> Self {
            Self {
                failures_left: Arc::new(Mutex::new(failures)),
                calls: Arc::new(Mutex::new(0)),
            }
        }
    }

    impl Service<Request<JobEnvelope, RedisContext>> for FlakyService {
        type Response = ();
        type Error = Error;
        type Future = Pin<Box<dyn Future<Output = Result<(), Error>> + Send>>;

        fn poll_ready(
            &mut self,
            _cx: &mut std::task::Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, _req: Request<JobEnvelope, RedisContext>) -> Self::Future {
            let failures_left = self.failures_left.clone();
            let calls = self.calls.clone();
            Box::pin(async move {
                *calls.lock().expect("calls lock") += 1;
                let mut left = failures_left.lock().expect("failures lock");
                if *left > 0 {
                    *left -= 1;
                    Err(Error::Failed(Arc::new(
                        Box::new(std::io::Error::other("boom")) as BoxDynError,
                    )))
                } else {
                    Ok(())
                }
            })
        }
    }

    fn envelope() -> JobEnvelope {
        JobEnvelope::new(Uuid::now_v7(), "ping", serde_json::json!({"probe": true}))
    }

    fn request() -> Request<JobEnvelope, RedisContext> {
        Request::new(envelope())
    }

    /// A fast policy for tests: millisecond delays, tiny attempt counts.
    fn fast_policy(max_attempts: usize) -> RetryPolicy {
        RetryPolicy {
            max_attempts,
            base_delays: [
                Duration::from_millis(10),
                Duration::from_millis(20),
                Duration::from_millis(30),
            ],
        }
    }

    #[test]
    fn for_queue_sets_per_queue_max_attempts() {
        assert_eq!(RetryPolicy::for_queue("pipeline").max_attempts, 5);
        assert_eq!(RetryPolicy::for_queue("cleanup").max_attempts, 3);
        assert_eq!(RetryPolicy::for_queue("email").max_attempts, 5);
        assert_eq!(RetryPolicy::for_queue("reconciliation").max_attempts, 3);
        assert_eq!(RetryPolicy::for_queue("mystery").max_attempts, 5);
    }

    #[test]
    fn backoff_schedule_is_one_five_sixty_minutes() {
        let policy = RetryPolicy::for_queue("pipeline");
        assert_eq!(policy.base_delay(0), Duration::from_secs(60));
        assert_eq!(policy.base_delay(1), Duration::from_secs(5 * 60));
        assert_eq!(policy.base_delay(2), Duration::from_secs(60 * 60));
        assert_eq!(policy.base_delay(3), Duration::from_secs(60 * 60));
        assert_eq!(policy.base_delay(99), Duration::from_secs(60 * 60));
    }

    #[test]
    fn should_retry_stops_at_max_attempts() {
        let policy = RetryPolicy::for_queue("pipeline"); // 5
        assert!(policy.should_retry(0));
        assert!(policy.should_retry(3));
        assert!(
            !policy.should_retry(4),
            "attempt 4 is the 5th run — no more"
        );
        assert!(!policy.should_retry(5));
        let cleanup = RetryPolicy::for_queue("cleanup"); // 3
        assert!(cleanup.should_retry(1));
        assert!(!cleanup.should_retry(2));
    }

    #[test]
    fn jitter_stays_within_a_quarter_either_way_and_is_deterministic() {
        let policy = RetryPolicy::for_queue("pipeline");
        for attempt in 0..4 {
            for i in 0..50 {
                let job_id = Uuid::from_u128(i as u128 * 0x9e37_79b9_7f4a_7c15 + 1);
                let delay = policy.delay_for(attempt, job_id);
                let base = policy.base_delay(attempt);
                assert!(
                    delay >= base.mul_f64(0.75) && delay <= base.mul_f64(1.25),
                    "delay {delay:?} outside ±25% of {base:?} (attempt {attempt})"
                );
                assert_eq!(
                    delay,
                    policy.delay_for(attempt, job_id),
                    "same (job, attempt) must give the same delay"
                );
            }
        }
        // ...and it actually spreads: not every job gets the same delay.
        let delays: Vec<_> = (0..20)
            .map(|i| policy.delay_for(0, Uuid::from_u128(i as u128 * 0x9e37_79b9_7f4a_7c15 + 1)))
            .collect();
        let first = delays[0];
        assert!(
            delays.iter().any(|d| *d != first),
            "jitter must spread delays, got all {first:?}"
        );
    }

    #[tokio::test]
    async fn service_retries_then_succeeds_and_records_events() {
        let sink = RecordingSink::default();
        let inner = FlakyService::new(2);
        let calls = inner.calls.clone();
        let metrics = Metrics::new();
        let hooks = crate::queue_hooks::QueueHooks::new(metrics.queue().clone());
        let dlq = RecordingDeadLetterSink::default();
        let mut service = RetryService {
            inner,
            policy: fast_policy(5),
            sink: sink.clone(),
            hooks,
            dlq: Arc::new(dlq.clone()),
            queue: "pipeline",
        };
        let started = Instant::now();
        let outcome = service.call(request()).await;
        assert!(outcome.is_ok(), "the 3rd attempt succeeds");
        assert_eq!(*calls.lock().expect("calls lock"), 3, "1 run + 2 retries");
        // Two retries, ~10 ms + ~20 ms of backoff (jitter ±25%).
        let elapsed = started.elapsed();
        assert!(
            elapsed >= Duration::from_millis(20),
            "backoff sleeps must actually happen, elapsed {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "test must not take real minutes, elapsed {elapsed:?}"
        );
        let events = sink.events.lock().expect("events lock");
        assert_eq!(events.len(), 2, "one event per retry");
        assert_eq!(events[0].attempt, 0);
        assert_eq!(events[1].attempt, 1);
        assert!(events[0].delay >= Duration::from_millis(7));
        assert!(events[0].delay <= Duration::from_millis(13));
        assert_eq!(events[0].queue, "pipeline");
        assert_eq!(events[0].kind, "ping");
        assert!(events[0].error.contains("boom"));
        assert!(
            metrics
                .gather()
                .contains("detag_queue_retried_total{queue=\"pipeline\"} 2"),
            "two retries counted:\n{}",
            metrics.gather()
        );
    }

    #[tokio::test]
    async fn service_aborts_when_attempts_are_exhausted() {
        let sink = RecordingSink::default();
        let inner = FlakyService::new(99); // never succeeds
        let calls = inner.calls.clone();
        let metrics = Metrics::new();
        let hooks = crate::queue_hooks::QueueHooks::new(metrics.queue().clone());
        let dlq = RecordingDeadLetterSink::default();
        let mut service = RetryService {
            inner,
            policy: fast_policy(3),
            sink: sink.clone(),
            hooks,
            dlq: Arc::new(dlq.clone()),
            queue: "cleanup",
        };
        let outcome = service.call(request()).await;
        let err = outcome.expect_err("exhaustion must error");
        assert!(
            matches!(err, Error::Abort(_)),
            "exhaustion must Abort (→ dead set), got {err:?}"
        );
        assert!(
            err.to_string().contains("exhausted 3 attempts"),
            "the abort names the limit, got: {err}"
        );
        assert_eq!(
            *calls.lock().expect("calls lock"),
            3,
            "exactly max attempts"
        );
        let events = sink.events.lock().expect("events lock");
        assert_eq!(events.len(), 2, "events for the 2 retries, not the death");
        assert_eq!(events[1].attempt, 1);
        assert!(
            metrics
                .gather()
                .contains("detag_queue_dead_total{queue=\"cleanup\"} 1"),
            "the death counted:\n{}",
            metrics.gather()
        );
        assert!(
            metrics
                .gather()
                .contains("detag_queue_retried_total{queue=\"cleanup\"} 2"),
            "the 2 retries counted:\n{}",
            metrics.gather()
        );
        // The death is filed in the DLQ with its payload snapshot.
        let letters = dlq.letters.lock().expect("letters lock");
        assert_eq!(letters.len(), 1, "one dead letter");
        assert_eq!(letters[0].0, "cleanup", "filed under the queue");
        assert_eq!(letters[0].1.kind, "ping");
        assert_eq!(letters[0].2, "retries exhausted");
        assert!(
            letters[0].3.as_deref().unwrap_or_default().contains("boom"),
            "the last error travels with the letter: {:?}",
            letters[0].3
        );
    }

    #[tokio::test]
    async fn service_passes_through_a_handler_abort_without_retry() {
        // A handler that aborts directly (its own decision) must
        // not be retried by this layer.
        #[derive(Debug, Clone)]
        struct AbortingService;
        impl Service<Request<JobEnvelope, RedisContext>> for AbortingService {
            type Response = ();
            type Error = Error;
            type Future = Pin<Box<dyn Future<Output = Result<(), Error>> + Send>>;
            fn poll_ready(
                &mut self,
                _cx: &mut std::task::Context<'_>,
            ) -> Poll<Result<(), Self::Error>> {
                Poll::Ready(Ok(()))
            }
            fn call(&mut self, _req: Request<JobEnvelope, RedisContext>) -> Self::Future {
                Box::pin(async {
                    Err(Error::Abort(Arc::new(
                        Box::new(std::io::Error::other("nope")) as BoxDynError,
                    )))
                })
            }
        }
        let sink = RecordingSink::default();
        let metrics = Metrics::new();
        let hooks = crate::queue_hooks::QueueHooks::new(metrics.queue().clone());
        let dlq = RecordingDeadLetterSink::default();
        let mut service = RetryService {
            inner: AbortingService,
            policy: fast_policy(5),
            sink: sink.clone(),
            hooks,
            dlq: Arc::new(dlq.clone()),
            queue: "pipeline",
        };
        let outcome = service.call(request()).await;
        assert!(matches!(outcome, Err(Error::Abort(_))));
        assert!(
            sink.events.lock().expect("events lock").is_empty(),
            "an abort is not a retry — no event"
        );
        // ...but the abort is still a death: it is filed in the DLQ.
        let letters = dlq.letters.lock().expect("letters lock");
        assert_eq!(letters.len(), 1, "one dead letter for the abort");
        assert_eq!(letters[0].2, "aborted by handler");
    }
}
