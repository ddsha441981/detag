//! detag-worker::mail_drain — the outbox drain loop (P1-028).
//!
//! The api never sends mail directly: it enqueues rows in
//! `email_outbox` and this loop — one spawned task in the
//! worker — claims due rows, renders the named template and
//! sends the mail over SMTP (see `detag_mailer::drain_once`).
//! One pass runs immediately at startup, then every
//! `PF_MAILER_DRAIN_INTERVAL_SECS` (default 30 s) until the
//! shutdown signal arrives. A pass that fails at the claim
//! (the database hiccuped) is logged and retried next
//! interval — mail is best-effort, it must never kill the
//! queue worker. Per-row failures are recorded on the row by
//! the drain itself (backoff or park) and never propagate.

use detag_core::db::DbPool;
use detag_mailer::{drain_once, DrainConfig, MailTransport, TemplateEngine};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;

/// Run the drain loop until `shutdown` fires: one pass now,
/// then one pass per interval. Returns when the signal
/// arrives (mid-pass the current pass finishes first — a
/// pass is seconds at most, so the shutdown drain window is
/// not meaningfully extended).
pub async fn run_mail_drain<T: MailTransport>(
    pool: DbPool,
    transport: Arc<T>,
    engine: Arc<TemplateEngine>,
    config: DrainConfig,
    shutdown: Arc<Notify>,
) {
    let interval = Duration::from_secs(config.interval_secs);
    loop {
        // `.as_ref()`, not `&transport`: drain_once is generic
        // over `T: MailTransport`, so `&Arc<T>` would unify `T`
        // as `Arc<T>` (which does not implement the trait) instead
        // of deref-coercing to `&T`.
        match drain_once(
            &pool,
            transport.as_ref(),
            engine.as_ref(),
            &config,
            chrono::Utc::now(),
        )
        .await
        {
            Ok(outcome) => {
                if outcome.claimed > 0 {
                    tracing::info!(
                        claimed = outcome.claimed,
                        sent = outcome.sent,
                        transient_failed = outcome.transient_failed,
                        permanent_failed = outcome.permanent_failed,
                        "outbox drain pass finished"
                    );
                }
            }
            Err(err) => {
                // The claim itself failed (database down): the
                // next interval retries. Per-row failures never
                // reach here — the drain records them on the row.
                tracing::error!(error = %err, "outbox drain pass failed — retrying next interval");
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(interval) => {}
            _ = shutdown.notified() => {
                tracing::info!("outbox drain loop stopping");
                return;
            }
        }
    }
}
