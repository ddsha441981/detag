//! Nightly schedule (P1-082, FR-P-07).
//!
//! The worker's nightly jobs run on a wall-clock schedule,
//! not on an interval: one enqueue per day at
//! `PF_WORKER_NIGHTLY_HOUR_UTC` (default 02:00 UTC — after
//! the UTC day has turned over, before the day's traffic).
//! [`run_nightly_schedule`] sleeps until the next run,
//! enqueues the nightly kinds, and repeats until shutdown.
//!
//! Today the schedule enqueues exactly one kind:
//! `reconciliation` (the P1-082 job — redis vs DB drift, DB
//! wins, audit on corrections beyond threshold). `dunning`
//! and `catalog_sync` are NOT enqueued yet: their handlers
//! are still stubs, and enqueueing a job a stub would
//! log-and-ack is not a nightly run — it is a dropped job.
//! Their enqueue lines land with their handlers (dunning's
//! engine lives in `detag-api`, which the worker does not
//! depend on; catalog sync needs a provider in the worker
//! context). The schedule is built so adding a kind is one
//! `push` call each.
//!
//! A missed night is not lost work: reconciliation is
//! idempotent and the next night's run covers the same
//! window (today + yesterday) — drift a skipped night
//! leaves behind is repaired the night after. An enqueue
//! failure is logged at error level and the schedule waits
//! for the next run; it never retries in a tight loop.

use crate::envelope::JobEnvelope;
use crate::workers::{apalis_config, WorkerConfig};
use apalis::prelude::*;
use apalis_redis::RedisStorage;
use chrono::{DateTime, Duration, Utc};
use serde_json::json;
use std::sync::Arc;
use tokio::sync::Notify;
use uuid::Uuid;

/// The queue the nightly jobs are enqueued on.
pub const NIGHTLY_QUEUE: &str = "reconciliation";

/// The next run after `now`: today at `hour_utc:00` when
/// that is still ahead, else tomorrow at the same hour.
/// Pure, so the boundary rule is unit-testable without
/// sleeping. `now` exactly at the hour runs tomorrow — a
/// run that fires exactly on the hour must not immediately
/// re-fire. An out-of-range hour (impossible after boot
/// validation, `PF_WORKER_NIGHTLY_HOUR_UTC` is 0..=23)
/// falls back to tomorrow at midnight.
pub fn next_run_at(now: DateTime<Utc>, hour_utc: u32) -> DateTime<Utc> {
    let today = now.date_naive();
    let today_at = today
        .and_hms_opt(hour_utc, 0, 0)
        .map(|t| DateTime::from_naive_utc_and_offset(t, Utc));
    match today_at {
        Some(t) if t > now => t,
        Some(_) => tomorrow_at(today, hour_utc),
        None => tomorrow_at(today, 0),
    }
}

fn tomorrow_at(today: chrono::NaiveDate, hour_utc: u32) -> DateTime<Utc> {
    let date = today + Duration::days(1);
    let naive = date
        .and_hms_opt(hour_utc, 0, 0)
        .unwrap_or_else(|| date.and_hms_opt(0, 0, 0).expect("midnight is always valid"));
    DateTime::from_naive_utc_and_offset(naive, Utc)
}

/// Run the nightly schedule until `shutdown` fires: sleep
/// until the next run, enqueue the nightly kinds, repeat.
/// The storage is built once here (its own Redis
/// connection, the smoke-ping precedent) — the monitor's
/// per-queue storages are not shared out of
/// `build_monitor`. A dead Redis at startup is a logged
/// error and the task ends: the worker's other loops keep
/// running, and the next deploy restarts the schedule with
/// the process (the enqueue is retried the next night, not
/// in a loop — see the module docs).
pub async fn run_nightly_schedule(config: WorkerConfig, hour_utc: u32, shutdown: Arc<Notify>) {
    let conn = match apalis_redis::connect(config.redis_url.as_str()).await {
        Ok(conn) => conn,
        Err(err) => {
            tracing::error!(
                error = %err,
                "nightly schedule: cannot connect to Redis (PF_REDIS_URL) — nightly jobs will not run"
            );
            return;
        }
    };
    let mut storage = RedisStorage::new_with_config(conn, apalis_config(&config, NIGHTLY_QUEUE));
    loop {
        let now = Utc::now();
        let next = next_run_at(now, hour_utc);
        let delay = (next - now).to_std().unwrap_or_default();
        tracing::info!(
            next_run = %next,
            hour_utc,
            "nightly schedule: next run"
        );
        tokio::select! {
            () = tokio::time::sleep(delay) => {}
            () = shutdown.notified() => {
                tracing::info!("nightly schedule: stopping");
                return;
            }
        }
        let envelope = JobEnvelope::new(
            Uuid::now_v7(),
            "reconciliation",
            json!({"trigger": "nightly", "hour_utc": hour_utc}),
        );
        let job_id = envelope.job_id;
        match storage.push(envelope).await {
            Ok(_) => tracing::info!(
                job_id = %job_id,
                queue = NIGHTLY_QUEUE,
                "nightly schedule: reconciliation enqueued"
            ),
            Err(err) => tracing::error!(
                job_id = %job_id,
                error = %err,
                "nightly schedule: reconciliation enqueue failed — retrying next night"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn next_run_is_today_when_the_hour_is_ahead() {
        let now = Utc.with_ymd_and_hms(2026, 11, 19, 1, 0, 0).unwrap();
        assert_eq!(
            next_run_at(now, 2),
            Utc.with_ymd_and_hms(2026, 11, 19, 2, 0, 0).unwrap()
        );
    }

    #[test]
    fn next_run_is_tomorrow_when_the_hour_passed() {
        let now = Utc.with_ymd_and_hms(2026, 11, 19, 3, 0, 0).unwrap();
        assert_eq!(
            next_run_at(now, 2),
            Utc.with_ymd_and_hms(2026, 11, 20, 2, 0, 0).unwrap()
        );
    }

    #[test]
    fn next_run_is_tomorrow_at_the_hour_itself() {
        // Exactly 02:00:00 runs tomorrow — the run that just
        // fired must not immediately re-fire.
        let now = Utc.with_ymd_and_hms(2026, 11, 19, 2, 0, 0).unwrap();
        assert_eq!(
            next_run_at(now, 2),
            Utc.with_ymd_and_hms(2026, 11, 20, 2, 0, 0).unwrap()
        );
    }

    #[test]
    fn next_run_crosses_midnight_and_months() {
        let now = Utc.with_ymd_and_hms(2026, 11, 30, 23, 30, 0).unwrap();
        assert_eq!(
            next_run_at(now, 2),
            Utc.with_ymd_and_hms(2026, 12, 1, 2, 0, 0).unwrap()
        );
        // Hour 0 from 00:30 runs tomorrow at midnight.
        let now = Utc.with_ymd_and_hms(2026, 11, 19, 0, 30, 0).unwrap();
        assert_eq!(
            next_run_at(now, 0),
            Utc.with_ymd_and_hms(2026, 11, 20, 0, 0, 0).unwrap()
        );
    }
}
