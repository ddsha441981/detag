//! SSE pubsub (P0-050): one Redis channel per job id, carrying the
//! live job events the api serves as SSE (`GET /v1/jobs/{id}/events`,
//! api spec; FR-J-04).
//!
//! - [`publish`] — publish one [`JobEvent`] on the job's channel
//!   (`pf:pubsub:job:{id}`, [`crate::redis::keys::job_events_channel`]).
//!   Uses the pool: a publish is one short command.
//! - [`subscribe`] — subscribe to a job's channel and get a
//!   [`JobEventStream`]. Takes the Redis URL, NOT the pool: a
//!   subscription holds a dedicated connection for its whole
//!   lifetime, and holding a pooled connection per SSE client
//!   would occupy a pool slot per client. The stream is backed by
//!   a bounded channel ([`SUBSCRIBER_BUFFER`]): when a slow
//!   consumer fills it, the pump DROPS the event and logs a
//!   warning (a slow SSE client must never block the publisher
//!   or grow memory without bound) — see
//!   [`JobEventStream::dropped_count`].
//!
//! The SSE framing itself (`event: ...\ndata: ...`, the 15 s
//! heartbeat) is the HTTP layer's job (Phase 1/4) — this module
//! only moves typed events. The durable event log is the
//! `job_events` table (append-only); pubsub carries the live
//! feed only — a subscriber that connects late sees no history.

use crate::redis::keys::job_events_channel;
use crate::redis::RedisPool;
use deadpool_redis::redis::{cmd, Client, Msg};
use futures_core::Stream;
use serde::{Deserialize, Serialize};
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use thiserror::Error;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use uuid::Uuid;

/// Recommended subscriber buffer: 64 events. A job emits a
/// progress event per pipeline step (tens of events per job), so
/// 64 absorbs any burst a healthy SSE client cannot keep up with
/// momentarily; a slower client starts dropping after that.
pub const SUBSCRIBER_BUFFER: usize = 64;

/// One live job event. Serializes adjacently-tagged, so the JSON
/// on the wire is exactly the api spec's SSE payload shape:
/// `{"event":"stage","data":{"job_id":"...","stage":"detect","progress":0.34}}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", content = "data", rename_all = "snake_case")]
pub enum JobEvent {
    /// Progress tick: `job_id` is this far through (0.0–1.0).
    Progress { job_id: Uuid, progress: f32 },
    /// Pipeline stage change (with the progress at the change).
    Stage {
        job_id: Uuid,
        stage: String,
        progress: f32,
    },
    /// The job finished: `status` is `succeeded`, `result_url`
    /// is the result endpoint, `gate` is the quality gate
    /// (`pass`/`fail`).
    Done {
        job_id: Uuid,
        status: String,
        result_url: String,
        gate: String,
    },
    /// The job failed: `status` is `failed`/`quality_failed`,
    /// `code` is the stable `PF-` code, `report_url` is the
    /// quality report.
    Failed {
        job_id: Uuid,
        status: String,
        code: String,
        report_url: String,
    },
}

impl JobEvent {
    /// The job this event belongs to (and whose channel it travels on).
    pub fn job_id(&self) -> Uuid {
        match self {
            JobEvent::Progress { job_id, .. }
            | JobEvent::Stage { job_id, .. }
            | JobEvent::Done { job_id, .. }
            | JobEvent::Failed { job_id, .. } => *job_id,
        }
    }

    /// The SSE event name: `progress` | `stage` | `done` | `failed`.
    pub fn event_name(&self) -> &'static str {
        match self {
            JobEvent::Progress { .. } => "progress",
            JobEvent::Stage { .. } => "stage",
            JobEvent::Done { .. } => "done",
            JobEvent::Failed { .. } => "failed",
        }
    }
}

/// Pubsub failures.
#[derive(Debug, Error)]
pub enum PubSubError {
    /// Bad call (e.g. a subscriber buffer of 0).
    #[error("invalid pubsub call: {0}")]
    Invalid(String),
    /// Checkout from the pool failed (publish path).
    #[error("redis pool error: {0}")]
    Pool(#[from] deadpool_redis::PoolError),
    /// A Redis command failed, or the subscriber connection failed.
    #[error("redis error: {0}")]
    Redis(#[from] deadpool_redis::redis::RedisError),
    /// The event could not be serialized (cannot happen for this
    /// enum — the variant exists so the type stays honest).
    #[error("event is not serializable: {0}")]
    Encode(String),
}

/// Publish one event on its job's channel. Returns the number of
/// subscribers that received it (0 when nobody is listening —
/// normal for a job nobody is watching).
pub async fn publish(pool: &RedisPool, event: &JobEvent) -> Result<i64, PubSubError> {
    let mut conn = pool.get().await?;
    let channel = job_events_channel(&event.job_id());
    let payload = serde_json::to_string(event).map_err(|e| PubSubError::Encode(e.to_string()))?;
    let receivers: i64 = cmd("PUBLISH")
        .arg(&channel)
        .arg(&payload)
        .query_async(&mut conn)
        .await?;
    Ok(receivers)
}

/// A live stream of one job's events. Poll it (or `while let
/// Some(event) = stream.next().await`) until it ends — it ends
/// when the Redis connection drops or the stream is dropped.
/// Dropping the stream aborts the pump task and closes the
/// Redis subscription.
#[derive(Debug)]
pub struct JobEventStream {
    rx: mpsc::Receiver<JobEvent>,
    pump: JoinHandle<()>,
    dropped: Arc<AtomicUsize>,
}

impl JobEventStream {
    /// Events dropped because the consumer was too slow (the
    /// bounded buffer filled). A slow SSE client loses events —
    /// by design; it can re-sync by polling the job (FR-J-04
    /// allows polling as well as SSE).
    pub fn dropped_count(&self) -> usize {
        self.dropped.load(Ordering::SeqCst)
    }

    /// The next event, or `None` when the stream ends (the Redis
    /// connection dropped). The simple way to consume the stream
    /// without the `Stream` trait: `while let Some(event) =
    /// stream.next_event().await`.
    pub async fn next_event(&mut self) -> Option<JobEvent> {
        self.rx.recv().await
    }
}

impl Stream for JobEventStream {
    type Item = JobEvent;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.rx.poll_recv(cx)
    }
}

impl Drop for JobEventStream {
    fn drop(&mut self) {
        self.pump.abort();
    }
}

/// Subscribe to `job_id`'s channel. `capacity` is the bounded
/// buffer between the Redis pump and the consumer
/// ([`SUBSCRIBER_BUFFER`] is the recommended value; must be >= 1).
pub async fn subscribe(
    redis_url: &str,
    job_id: &Uuid,
    capacity: usize,
) -> Result<JobEventStream, PubSubError> {
    if capacity == 0 {
        return Err(PubSubError::Invalid(
            "subscriber buffer capacity must be >= 1".to_string(),
        ));
    }
    let client = Client::open(redis_url)?;
    let mut pubsub = client.get_async_pubsub().await?;
    let channel = job_events_channel(job_id);
    pubsub.subscribe(&channel).await?;
    // `split()` gives owned halves: the stream carries the
    // messages, the sink carries the subscription. The sink must
    // stay alive for the pump's lifetime (it is moved into the
    // task below) — dropping it would close the subscription.
    let (sink, stream) = pubsub.split();
    let mut messages = Box::pin(stream);
    let (tx, rx) = mpsc::channel::<JobEvent>(capacity);
    let dropped = Arc::new(AtomicUsize::new(0));
    let dropped_clone = Arc::clone(&dropped);
    let job_id = *job_id;
    let pump = tokio::spawn(async move {
        use std::future::poll_fn;
        use tokio::sync::mpsc::error::TrySendError;
        let _sink = sink;
        // `on_message()` yields a bare `Stream` (no `.next()` —
        // that is `StreamExt`, a second crate); poll it directly.
        while let Some(msg) = poll_fn(|cx| messages.as_mut().poll_next(cx)).await {
            let event = match parse_message(&msg) {
                Some(event) => event,
                None => continue,
            };
            match tx.try_send(event) {
                Ok(()) => {}
                Err(TrySendError::Full(event)) => {
                    dropped_clone.fetch_add(1, Ordering::SeqCst);
                    tracing::warn!(
                        job_id = %job_id,
                        event = event.event_name(),
                        "dropping SSE event: slow consumer (bounded buffer full)"
                    );
                }
                Err(TrySendError::Closed(_)) => break,
            }
        }
    });
    Ok(JobEventStream { rx, pump, dropped })
}

/// Parse one Redis message into a [`JobEvent`]. A malformed
/// message is logged and skipped — one bad publisher must not
/// kill the stream for every other event.
fn parse_message(msg: &Msg) -> Option<JobEvent> {
    let payload: String = match msg.get_payload() {
        Ok(payload) => payload,
        Err(err) => {
            tracing::warn!("ignoring non-string pubsub message: {err}");
            return None;
        }
    };
    match serde_json::from_str(&payload) {
        Ok(event) => Some(event),
        Err(err) => {
            tracing::warn!("ignoring malformed pubsub message: {err}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job_id() -> Uuid {
        Uuid::parse_str("0198c8d2-0000-7000-8000-000000000001").expect("test uuid parses")
    }

    #[test]
    fn wire_shape_matches_the_api_spec() {
        // The api spec's example payloads, verbatim (modulo key order).
        let stage = JobEvent::Stage {
            job_id: job_id(),
            stage: "detect".to_string(),
            progress: 0.34,
        };
        let json = serde_json::to_value(&stage).expect("serializes");
        assert_eq!(json["event"], "stage");
        assert_eq!(
            json["data"]["job_id"],
            "0198c8d2-0000-7000-8000-000000000001"
        );
        assert_eq!(json["data"]["stage"], "detect");
        assert!((json["data"]["progress"].as_f64().expect("number") - 0.34).abs() < 0.001);
        assert_eq!(stage.event_name(), "stage");
        assert_eq!(stage.job_id(), job_id());

        let done = JobEvent::Done {
            job_id: job_id(),
            status: "succeeded".to_string(),
            result_url: "/v1/jobs/0198c8d2-0000-7000-8000-000000000001/result".to_string(),
            gate: "pass".to_string(),
        };
        let json = serde_json::to_value(&done).expect("serializes");
        assert_eq!(json["event"], "done");
        assert_eq!(json["data"]["status"], "succeeded");
        assert_eq!(json["data"]["gate"], "pass");

        let failed = JobEvent::Failed {
            job_id: job_id(),
            status: "quality_failed".to_string(),
            code: "PF-4002".to_string(),
            report_url: "https://example.invalid/report".to_string(),
        };
        assert_eq!(failed.event_name(), "failed");

        // Round-trip: the wire form parses back to the same event.
        for event in [stage, done, failed] {
            let text = serde_json::to_string(&event).expect("serializes");
            let back: JobEvent = serde_json::from_str(&text).expect("parses");
            assert_eq!(back, event);
        }
    }

    #[tokio::test]
    async fn zero_capacity_is_rejected_without_connecting() {
        let err = subscribe("redis://127.0.0.1:1", &job_id(), 0)
            .await
            .expect_err("capacity 0 must be rejected");
        assert!(matches!(err, PubSubError::Invalid(_)));
    }
}
