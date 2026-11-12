//! Job registry (P0-076, E0.8): typed kind dispatch + quarantine.
//!
//! The registry maps a job's [`JobKind`] to its handler. The
//! envelope carries `kind` as a string on the wire (see
//! `envelope.rs`); the registry parses it here, so an unknown
//! kind survives serde and reaches this dispatch instead of
//! failing at the codec.
//!
//! Known kinds in Phase 0 are stubs: `ping` is the smoke probe
//! (P0-082 enqueues it); `pipeline`, `cleanup`, `email` and
//! `reconciliation` get their real handlers in later phases
//! (pipeline in Phase 2, the rest with the features that own
//! them). A stub handler only logs and acks — it never claims
//! work happened.
//!
//! An unknown kind is quarantined, never executed: the job is
//! acknowledged (no retry storm — retrying a kind this build
//! does not know is pointless), the quarantine is counted on
//! `detag_queue_quarantined_total{queue}` and logged at error
//! level with the job id and the unknown kind string (that is
//! the alert surface — error logs feed alerting). The kind
//! string is untrusted input: it is logged, never used as a
//! metric label. Persisting the payload snapshot to the DLQ
//! queue lands in P0-078; until then the log line + counter are
//! the quarantine record.

use crate::envelope::JobEnvelope;
use crate::workers::WorkerContext;
use apalis::prelude::*;

/// The known job kinds, plus the unknown-kind carrier.
///
/// The wire string is snake_case (`ping`, `pipeline`, ...);
/// parsing is case-sensitive — a producer that sends `Ping`
/// gets a quarantine, not a silent rename, because the kind
/// is a contract, not a suggestion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobKind {
    /// Smoke probe (P0-082): enqueued by `just smoke`, asserted done.
    Ping,
    /// Image/video pipeline job (real handler in Phase 2).
    Pipeline,
    /// Artifact/result cleanup job.
    Cleanup,
    /// Transactional email job.
    Email,
    /// Billing/storage reconciliation job.
    Reconciliation,
    /// A kind this build does not know — quarantined, never run.
    /// Carries the raw string for the log line.
    Unknown(String),
}

impl JobKind {
    /// The wire string for a known kind; the raw string for `Unknown`.
    pub fn as_str(&self) -> &str {
        match self {
            JobKind::Ping => "ping",
            JobKind::Pipeline => "pipeline",
            JobKind::Cleanup => "cleanup",
            JobKind::Email => "email",
            JobKind::Reconciliation => "reconciliation",
            JobKind::Unknown(raw) => raw,
        }
    }

    /// Parse the wire string. Anything unlisted is `Unknown` —
    /// never an error, so the registry can quarantine it.
    pub fn parse(raw: &str) -> Self {
        match raw {
            "ping" => JobKind::Ping,
            "pipeline" => JobKind::Pipeline,
            "cleanup" => JobKind::Cleanup,
            "email" => JobKind::Email,
            "reconciliation" => JobKind::Reconciliation,
            other => JobKind::Unknown(other.to_string()),
        }
    }
}

/// What dispatch did with the envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchOutcome {
    /// A handler ran (in Phase 0: a stub that logs and acks).
    Handled {
        /// The kind that was handled.
        kind: JobKind,
    },
    /// The kind is unknown: never executed, counted + logged,
    /// acknowledged. Carries the raw kind string for the caller.
    Quarantined {
        /// The unknown kind string.
        kind: String,
    },
}

/// The kind → handler map. Stateless today (every handler is a
/// free function below); kept as a struct so per-kind config
/// (retry, timeout) has a home in P0-077.
#[derive(Debug, Clone, Copy, Default)]
pub struct JobRegistry;

impl JobRegistry {
    /// An empty registry (all handlers are the Phase-0 stubs).
    pub fn new() -> Self {
        Self
    }

    /// Route one envelope to its handler, or quarantine it.
    ///
    /// A handler error propagates (`Err`) so apalis retries it
    /// under the P0-077 policy; a quarantine never errors — the
    /// job is acknowledged after counting + logging.
    pub async fn dispatch(
        &self,
        envelope: &JobEnvelope,
        ctx: &WorkerContext,
    ) -> Result<DispatchOutcome, Error> {
        let kind = JobKind::parse(&envelope.kind);
        match kind {
            JobKind::Ping => {
                handle_ping(envelope, ctx).await?;
                Ok(DispatchOutcome::Handled { kind })
            }
            JobKind::Pipeline => {
                handle_pipeline(envelope, ctx).await?;
                Ok(DispatchOutcome::Handled { kind })
            }
            JobKind::Cleanup => {
                handle_cleanup(envelope, ctx).await?;
                Ok(DispatchOutcome::Handled { kind })
            }
            JobKind::Email => {
                handle_email(envelope, ctx).await?;
                Ok(DispatchOutcome::Handled { kind })
            }
            JobKind::Reconciliation => {
                handle_reconciliation(envelope, ctx).await?;
                Ok(DispatchOutcome::Handled { kind })
            }
            JobKind::Unknown(raw) => {
                ctx.hooks.quarantined(ctx.queue);
                tracing::error!(
                    job_id = %envelope.job_id,
                    kind = %raw,
                    queue = ctx.queue,
                    "unknown job kind quarantined (never executed)"
                );
                Ok(DispatchOutcome::Quarantined { kind: raw })
            }
        }
    }
}

/// Phase-0 stub: the smoke probe only proves the loop works.
async fn handle_ping(envelope: &JobEnvelope, ctx: &WorkerContext) -> Result<(), Error> {
    tracing::info!(
        job_id = %envelope.job_id,
        queue = ctx.queue,
        "ping handled (stub)"
    );
    Ok(())
}

/// Phase-0 stub: the real pipeline handler lands in Phase 2.
async fn handle_pipeline(envelope: &JobEnvelope, ctx: &WorkerContext) -> Result<(), Error> {
    tracing::info!(
        job_id = %envelope.job_id,
        queue = ctx.queue,
        "pipeline handled (stub — real handler lands in Phase 2)"
    );
    Ok(())
}

/// Phase-0 stub: the real cleanup handler lands with retention.
async fn handle_cleanup(envelope: &JobEnvelope, ctx: &WorkerContext) -> Result<(), Error> {
    tracing::info!(
        job_id = %envelope.job_id,
        queue = ctx.queue,
        "cleanup handled (stub)"
    );
    Ok(())
}

/// Phase-0 stub: the real email handler lands with billing/mail.
async fn handle_email(envelope: &JobEnvelope, ctx: &WorkerContext) -> Result<(), Error> {
    tracing::info!(
        job_id = %envelope.job_id,
        queue = ctx.queue,
        "email handled (stub)"
    );
    Ok(())
}

/// Phase-0 stub: the real reconciliation handler lands with billing.
async fn handle_reconciliation(envelope: &JobEnvelope, ctx: &WorkerContext) -> Result<(), Error> {
    tracing::info!(
        job_id = %envelope.job_id,
        queue = ctx.queue,
        "reconciliation handled (stub)"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use detag_core::metrics::Metrics;

    fn context() -> WorkerContext {
        let metrics = Metrics::new();
        let hooks = crate::queue_hooks::QueueHooks::new(metrics.queue().clone());
        WorkerContext {
            queue: "pipeline",
            hooks,
            registry: JobRegistry::new(),
        }
    }

    fn envelope(kind: &str) -> JobEnvelope {
        JobEnvelope::new(
            uuid::Uuid::now_v7(),
            kind,
            serde_json::json!({"probe": true}),
        )
    }

    #[test]
    fn parse_round_trips_every_known_kind() {
        for (raw, kind) in [
            ("ping", JobKind::Ping),
            ("pipeline", JobKind::Pipeline),
            ("cleanup", JobKind::Cleanup),
            ("email", JobKind::Email),
            ("reconciliation", JobKind::Reconciliation),
        ] {
            assert_eq!(JobKind::parse(raw), kind, "parse {raw}");
            assert_eq!(kind.as_str(), raw, "as_str {raw:?}");
        }
    }

    #[test]
    fn parse_is_case_sensitive_and_unknown_is_preserved() {
        assert_eq!(
            JobKind::parse("Ping"),
            JobKind::Unknown("Ping".to_string()),
            "Ping (capital P) is not the ping kind"
        );
        assert_eq!(
            JobKind::parse("explode"),
            JobKind::Unknown("explode".to_string())
        );
        assert_eq!(JobKind::Unknown("explode".to_string()).as_str(), "explode");
    }

    #[tokio::test]
    async fn dispatch_handles_every_known_kind() {
        let ctx = context();
        for kind in ["ping", "pipeline", "cleanup", "email", "reconciliation"] {
            let outcome = JobRegistry::new()
                .dispatch(&envelope(kind), &ctx)
                .await
                .expect("known kinds dispatch cleanly");
            assert_eq!(
                outcome,
                DispatchOutcome::Handled {
                    kind: JobKind::parse(kind)
                },
                "dispatch {kind}"
            );
        }
    }

    #[tokio::test]
    async fn dispatch_quarantines_unknown_kind_without_running_it() {
        let ctx = context();
        let outcome = JobRegistry::new()
            .dispatch(&envelope("explode"), &ctx)
            .await
            .expect("quarantine acks, never errors");
        assert_eq!(
            outcome,
            DispatchOutcome::Quarantined {
                kind: "explode".to_string()
            }
        );
    }
}
