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
//! them). `catalog_sync` is the same shape (P1-061): the sync
//! engine + the manual admin trigger are real, the nightly
//! schedule (P1-082) enqueues `reconciliation` only until its
//! handler lands. `reconciliation` IS real since P1-082 —
//! when the context carries services the handler runs the
//! nightly repair; without services it stays a stub (a stub
//! handler only logs and acks — it never claims work
//! happened).
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
use std::sync::Arc;
use std::time::Duration;

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
    /// Billing/storage reconciliation job (P1-082): the
    /// nightly redis-vs-DB drift repair — DB wins, audit on
    /// corrections beyond the threshold. The handler is real
    /// when the context carries services, a stub otherwise.
    Reconciliation,
    /// Stripe products/prices catalog sync (P1-061): the nightly
    /// job (schedule: P1-082) + the manual admin trigger run the
    /// same sync engine; the handler here is a stub until the
    /// worker gains a provider + pool to run it with.
    CatalogSync,
    /// Dunning policy job (P1-070): the day-3/7 reminders + the
    /// day-14 subscription cancel (`services/dunning.rs` in the
    /// api crate is the real engine). The nightly schedule that
    /// enqueues it is P1-082's; the handler here is a stub until
    /// the worker gains a pool + clock + settings to run it with.
    Dunning,
    /// Weekly digest email (P1-101): the usage/plan/tips mail,
    /// once per user + week (`services/weekly_digest.rs` in
    /// the api crate is the real engine, flag-gated with the
    /// opt-out honored). No weekly schedule exists yet — the
    /// handler here is a stub until the worker gains a pool +
    /// settings to run it with, and the enqueue line lands
    /// with that schedule (the nightly doc's rule).
    WeeklyDigest,
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
            JobKind::CatalogSync => "catalog_sync",
            JobKind::Dunning => "dunning",
            JobKind::WeeklyDigest => "weekly_digest",
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
            "catalog_sync" => JobKind::CatalogSync,
            "dunning" => JobKind::Dunning,
            "weekly_digest" => JobKind::WeeklyDigest,
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
            JobKind::CatalogSync => {
                handle_catalog_sync(envelope, ctx).await?;
                Ok(DispatchOutcome::Handled { kind })
            }
            JobKind::Dunning => {
                handle_dunning(envelope, ctx).await?;
                Ok(DispatchOutcome::Handled { kind })
            }
            JobKind::WeeklyDigest => {
                handle_weekly_digest(envelope, ctx).await?;
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
///
/// Drain-drill seam (P0-081): a `ping` payload may carry
/// `sleep_ms` — the handler sleeps that long before acking, so
/// a drain test can catch a job in flight when SIGTERM lands.
/// Capped at 30 s: a drill must never turn a stub into a hang,
/// and a real handler's slowness is its own business. Absent
/// or non-numeric `sleep_ms` means no sleep; the value is
/// logged so a slow ping in production is never mysterious.
const PING_SLEEP_CAP_MS: u64 = 30_000;

/// The sleep a `ping` payload asks for, capped. Pure, so the
/// cap is unit-testable without sleeping.
fn ping_sleep_ms(payload: &serde_json::Value) -> u64 {
    payload
        .get("sleep_ms")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0)
        .min(PING_SLEEP_CAP_MS)
}

async fn handle_ping(envelope: &JobEnvelope, ctx: &WorkerContext) -> Result<(), Error> {
    let sleep_ms = ping_sleep_ms(&envelope.payload);
    if sleep_ms > 0 {
        tracing::info!(
            job_id = %envelope.job_id,
            queue = ctx.queue,
            sleep_ms,
            "ping sleeping (drain-drill seam)"
        );
        tokio::time::sleep(Duration::from_millis(sleep_ms)).await;
    }
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

/// P1-082: the nightly reconciliation job is REAL. When the
/// context carries services, the handler runs
/// `detag_core::reconciliation::run_reconciliation` (redis vs
/// DB drift, DB wins, audit on corrections beyond the
/// threshold) and logs the report; a job error propagates so
/// apalis retries it under the P0-077 policy (a failed list
/// fails the whole run, a failed user-day is counted inside
/// the report and never fails the job). Without services the
/// handler stays the Phase-0 stub — it logs and acks instead
/// of claiming a run happened (unit-test contexts build no
/// services; production always has them).
async fn handle_reconciliation(envelope: &JobEnvelope, ctx: &WorkerContext) -> Result<(), Error> {
    let Some(services) = ctx.services.as_deref() else {
        tracing::info!(
            job_id = %envelope.job_id,
            queue = ctx.queue,
            "reconciliation handled (stub — no services in this context)"
        );
        return Ok(());
    };
    let report = detag_core::reconciliation::run_reconciliation(
        &services.pool,
        &services.redis,
        Arc::clone(&services.clock),
        &services.reconcile_config,
    )
    .await
    .map_err(|err| Error::Failed(Arc::new(Box::new(err) as BoxDynError)))?;
    tracing::info!(
        job_id = %envelope.job_id,
        queue = ctx.queue,
        ran = report.ran,
        window_start = %report.window_start,
        window_end = %report.window_end,
        checked = report.checked,
        corrected = report.corrected,
        audited = report.audited,
        errors = report.errors,
        "reconciliation handled"
    );
    Ok(())
}

/// P1-061 stub: the catalog sync engine (`detag-billing::sync`)
/// and the manual admin trigger are real, but the worker has no
/// provider + pool in its context to run the sync with — and the
/// nightly schedule (P1-082) enqueues `reconciliation` only
/// until this handler lands. Until then, an enqueued
/// `catalog_sync` job logs and acks instead of claiming a sync
/// ran.
async fn handle_catalog_sync(envelope: &JobEnvelope, ctx: &WorkerContext) -> Result<(), Error> {
    tracing::info!(
        job_id = %envelope.job_id,
        queue = ctx.queue,
        "catalog_sync handled (stub — real handler lands with a provider in the worker context)"
    );
    Ok(())
}

/// P1-070 stub: the dunning engine (`detag-api::services
/// ::dunning::run_dunning`) is real, but the worker has no
/// pool + clock + settings in its context to run it with —
/// and the nightly schedule (P1-082) enqueues `reconciliation`
/// only until this handler lands. Until then, an enqueued
/// `dunning` job logs and acks instead of claiming a pass ran.
async fn handle_dunning(envelope: &JobEnvelope, ctx: &WorkerContext) -> Result<(), Error> {
    tracing::info!(
        job_id = %envelope.job_id,
        queue = ctx.queue,
        "dunning handled (stub — real handler lands with the dunning engine in the worker context)"
    );
    Ok(())
}

/// P1-101 stub: the digest engine (`detag-api::services
/// ::weekly_digest::send_weekly_digest`) is real, but the
/// worker has no pool + settings in its context to run it
/// with — and no weekly schedule exists to enqueue it. Until
/// then, an enqueued `weekly_digest` job logs and acks
/// instead of claiming a digest ran.
async fn handle_weekly_digest(envelope: &JobEnvelope, ctx: &WorkerContext) -> Result<(), Error> {
    tracing::info!(
        job_id = %envelope.job_id,
        queue = ctx.queue,
        "weekly_digest handled (stub — real handler lands with the digest engine in the worker context)"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use detag_core::metrics::Metrics;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Arc;

    fn context() -> WorkerContext {
        let metrics = Metrics::new();
        let hooks = crate::queue_hooks::QueueHooks::new(metrics.queue().clone());
        WorkerContext {
            queue: "pipeline",
            hooks,
            registry: JobRegistry::new(),
            dlq: Arc::new(crate::dlq::test_support::RecordingDeadLetterSink::default()),
            inflight: Arc::new(AtomicUsize::new(0)),
            // No services: the unit tests exercise the stub
            // paths; the real handler is proven by the worker
            // integration test (`tests/reconciliation.rs`).
            services: None,
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
            ("catalog_sync", JobKind::CatalogSync),
            ("dunning", JobKind::Dunning),
            ("weekly_digest", JobKind::WeeklyDigest),
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
        for kind in [
            "ping",
            "pipeline",
            "cleanup",
            "email",
            "reconciliation",
            "catalog_sync",
            "dunning",
            "weekly_digest",
        ] {
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

    #[test]
    fn ping_sleep_ms_is_absent_zero_and_capped() {
        assert_eq!(
            ping_sleep_ms(&serde_json::json!({})),
            0,
            "absent means no sleep"
        );
        assert_eq!(
            ping_sleep_ms(&serde_json::json!({"sleep_ms": 0})),
            0,
            "0 means no sleep"
        );
        assert_eq!(
            ping_sleep_ms(&serde_json::json!({"sleep_ms": 2000})),
            2000,
            "a small sleep passes through"
        );
        assert_eq!(
            ping_sleep_ms(&serde_json::json!({"sleep_ms": 999_999})),
            PING_SLEEP_CAP_MS,
            "an unbounded sleep is capped at 30 s"
        );
        assert_eq!(
            ping_sleep_ms(&serde_json::json!({"sleep_ms": "2000"})),
            0,
            "a non-numeric sleep_ms is not a sleep"
        );
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
