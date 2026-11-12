//! Dead-letter queue (P0-078, E0.8): where poison goes, with its
//! payload snapshot.
//!
//! Everything this build refuses to run or gives up on lands
//! here: unknown kinds, unsupported envelope versions,
//! `params_hash` mismatches (the P0-076 quarantines), and jobs
//! that exhausted their retries (the P0-077 dead). Each entry
//! is a [`DeadLetter`]: the full envelope (the payload
//! snapshot), the reason it died, and when — so an operator can
//! inspect and reprocess it without guessing what the job was.
//!
//! Storage is one Redis sorted set per queue,
//! `{prefix}:{queue}` (production prefix `pf:dlq`), score =
//! died-at unix time, member = the entry JSON. Newest first on
//! read. The set is capped at [`DLQ_MAX_ENTRIES`] entries (the
//! oldest fall off) so a poison storm cannot grow it without
//! bound.
//!
//! This is deliberately NOT apalis's dead set: apalis owns that
//! one (its key scheme and lifecycle are its internals), and
//! reading it would couple the DLQ to an apalis version. The
//! retry layer writes here AND aborts (so the job also lands in
//! apalis's dead set — the two agree), and the admin listing
//! (stub in this ticket, Phase 4 wires the UI) reads from here.
//!
//! Writes are best-effort: a DLQ failure is error-logged and
//! the job's fate does not change (the `job_events` row, the
//! counters and the log line are the fallback record). A DLQ
//! write must never fail a job that is already dead.

use crate::envelope::JobEnvelope;
use chrono::{DateTime, Utc};
use deadpool_redis::redis::AsyncCommands;
use detag_core::redis::RedisPool;
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::pin::Pin;
use uuid::Uuid;

/// The production key prefix (`pf:dlq:{queue}`).
pub const DLQ_PREFIX: &str = "pf:dlq";

/// The DLQ keeps at most this many entries per queue; older
/// ones fall off (see [`RedisDeadLetterSink::quarantine`]).
pub const DLQ_MAX_ENTRIES: i64 = 1000;

/// Reading the DLQ can fail two ways: the pool checkout or the
/// Redis command. Writes never surface this — they are
/// best-effort (see the module docs).
#[derive(Debug, thiserror::Error)]
pub enum DlqError {
    /// The pool could not hand out a connection.
    #[error("DLQ pool error: {0}")]
    Pool(#[from] deadpool_redis::PoolError),
    /// The Redis command failed.
    #[error("DLQ redis error: {0}")]
    Redis(#[from] deadpool_redis::redis::RedisError),
}

/// One dead letter: the job, why it died, and when. The
/// `envelope` is the payload snapshot — the exact JSON the
/// worker refused or gave up on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeadLetter {
    /// The Detag job that died.
    pub job_id: Uuid,
    /// The queue it ran on.
    pub queue: String,
    /// The kind it carried (may be an unknown kind string).
    pub kind: String,
    /// Why it died (`unknown kind …`, `retries exhausted`, …).
    pub reason: String,
    /// The last handler error, when the death was a failure
    /// (retry exhaustion). `None` for quarantines.
    pub error: Option<String>,
    /// When it died.
    pub died_at: DateTime<Utc>,
    /// The full envelope — the payload snapshot.
    pub envelope: JobEnvelope,
}

impl DeadLetter {
    /// The Redis key for `queue` under `prefix`
    /// (`{prefix}:{queue}`).
    pub fn key(prefix: &str, queue: &str) -> String {
        format!("{prefix}:{queue}")
    }
}

/// Where dead letters go. Dyn-compatible (the method returns a
/// boxed future) so [`WorkerContext`](crate::workers::WorkerContext)
/// and the retry layer can hold it behind an `Arc`. `Debug` is
/// required so the context can derive it.
pub trait DeadLetterSink: Send + Sync + std::fmt::Debug + 'static {
    /// Record `envelope` as dead on `queue`, with `reason` and
    /// the last handler `error` (if any). Best-effort: an
    /// implementation logs its own failure and never propagates
    /// it — the caller's `await` always succeeds.
    fn quarantine(
        &self,
        envelope: &JobEnvelope,
        queue: &str,
        reason: &str,
        error: Option<&str>,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
}

/// The Redis sink: one sorted set per queue (see the module
/// docs). `prefix` is `pf:dlq` in production; tests pass their
/// own prefix so test letters never land in the dev DLQ.
#[derive(Debug, Clone)]
pub struct RedisDeadLetterSink {
    pool: RedisPool,
    prefix: String,
}

impl RedisDeadLetterSink {
    /// A sink writing to `pool` under `prefix`.
    pub fn new(pool: RedisPool, prefix: impl Into<String>) -> Self {
        Self {
            pool,
            prefix: prefix.into(),
        }
    }

    /// The Redis key for `queue`.
    pub fn key(&self, queue: &str) -> String {
        DeadLetter::key(&self.prefix, queue)
    }

    /// List the dead letters on `queue`, newest first, at most
    /// `limit` entries. Entries whose JSON no longer parses are
    /// skipped (logged) rather than failing the whole listing.
    pub async fn list_dead(&self, queue: &str, limit: usize) -> Result<Vec<DeadLetter>, DlqError> {
        let mut conn = self.pool.get().await?;
        let members: Vec<String> = conn
            .zrevrange(self.key(queue), 0, limit as isize - 1)
            .await?;
        let mut out = Vec::with_capacity(members.len());
        for member in members {
            match serde_json::from_str::<DeadLetter>(&member) {
                Ok(letter) => out.push(letter),
                Err(err) => {
                    tracing::warn!(
                        queue = queue,
                        error = %err,
                        "dead-letter entry does not parse — skipped"
                    );
                }
            }
        }
        Ok(out)
    }

    /// How many dead letters `queue` holds.
    pub async fn count_dead(&self, queue: &str) -> Result<u64, DlqError> {
        let mut conn = self.pool.get().await?;
        let count: u64 = conn.zcard(self.key(queue)).await?;
        Ok(count)
    }
}

impl DeadLetterSink for RedisDeadLetterSink {
    fn quarantine(
        &self,
        envelope: &JobEnvelope,
        queue: &str,
        reason: &str,
        error: Option<&str>,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        let letter = DeadLetter {
            job_id: envelope.job_id,
            queue: queue.to_string(),
            kind: envelope.kind.clone(),
            reason: reason.to_string(),
            error: error.map(str::to_string),
            died_at: Utc::now(),
            envelope: envelope.clone(),
        };
        let key = self.key(queue);
        let pool = self.pool.clone();
        Box::pin(async move {
            let member = match serde_json::to_string(&letter) {
                Ok(member) => member,
                Err(err) => {
                    tracing::error!(
                        job_id = %letter.job_id,
                        error = %err,
                        "dead letter does not serialize — not recorded"
                    );
                    return;
                }
            };
            let score = letter.died_at.timestamp();
            let result: Result<(), DlqError> = async {
                let mut conn = pool.get().await?;
                let _: () = conn.zadd(&key, member, score).await?;
                let _: () = conn
                    .zremrangebyrank(&key, 0, (-DLQ_MAX_ENTRIES - 1) as isize)
                    .await?;
                Ok(())
            }
            .await;
            if let Err(err) = result {
                tracing::error!(
                    job_id = %letter.job_id,
                    queue = %letter.queue,
                    error = %err,
                    "dead letter not recorded in the DLQ (best-effort)"
                );
            }
        })
    }
}

/// A sink that does nothing — for contexts where no Redis exists.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopDeadLetterSink;

impl DeadLetterSink for NoopDeadLetterSink {
    fn quarantine(
        &self,
        _envelope: &JobEnvelope,
        _queue: &str,
        _reason: &str,
        _error: Option<&str>,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async {})
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    //! Test sinks: in-memory recording, shared by the worker
    //! unit tests.

    use super::*;
    use std::sync::{Arc, Mutex};

    /// One recorded quarantine: (queue, envelope, reason, error).
    pub type RecordedLetter = (String, JobEnvelope, String, Option<String>);

    /// Records every quarantine in order.
    #[derive(Debug, Clone, Default)]
    pub struct RecordingDeadLetterSink {
        /// One entry per quarantine, in order.
        pub letters: Arc<Mutex<Vec<RecordedLetter>>>,
    }

    impl DeadLetterSink for RecordingDeadLetterSink {
        fn quarantine(
            &self,
            envelope: &JobEnvelope,
            queue: &str,
            reason: &str,
            error: Option<&str>,
        ) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
            let letters = self.letters.clone();
            let envelope = envelope.clone();
            let queue = queue.to_string();
            let reason = reason.to_string();
            let error = error.map(str::to_string);
            Box::pin(async move {
                letters
                    .lock()
                    .expect("letters lock")
                    .push((queue, envelope, reason, error));
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope(kind: &str) -> JobEnvelope {
        JobEnvelope::new(
            Uuid::now_v7(),
            kind,
            serde_json::json!({"image": "s3://detag-dev/in.png"}),
        )
    }

    #[test]
    fn dead_letter_round_trips_through_json() {
        let env = envelope("pipeline");
        let letter = DeadLetter {
            job_id: env.job_id,
            queue: "pipeline".to_string(),
            kind: "pipeline".to_string(),
            reason: "retries exhausted".to_string(),
            error: Some("boom".to_string()),
            died_at: Utc::now(),
            envelope: env,
        };
        let json = serde_json::to_string(&letter).expect("letter serializes");
        assert!(json.contains("retries exhausted"), "reason in JSON: {json}");
        assert!(
            json.contains("s3://detag-dev/in.png"),
            "payload snapshot in JSON: {json}"
        );
        let back: DeadLetter = serde_json::from_str(&json).expect("letter deserializes");
        assert_eq!(back, letter);
    }

    #[test]
    fn key_is_prefix_plus_queue() {
        assert_eq!(DeadLetter::key("pf:dlq", "pipeline"), "pf:dlq:pipeline");
        assert_eq!(
            DeadLetter::key("pf:test-dlq-x", "email"),
            "pf:test-dlq-x:email"
        );
    }
}
