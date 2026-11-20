//! Nightly usage reconciliation (P1-082, FR-P-07).
//!
//! The meter (`UsageMeter`, P1-050) writes every record twice
//! — the Redis day-hash first, the durable `usage_counters`
//! row second — so a failure between the two legs leaves the
//! stores disagreeing. This job is the nightly repair: for
//! every user-day in the window it compares Redis against the
//! DB, the DB wins (the P1-050 `reconcile_day` CAS — the
//! durable row is never wiped, though a record's hash
//! increment can be transiently set back and the next run
//! repairs it; ADR-0048), and the run
//! produces an anomaly report — the corrections whose drift
//! reaches the audit thresholds are written to the audit trail
//! (`billing.usage.reconciled`), one row per anomaly.
//!
//! # The window
//!
//! Today + yesterday only ([`RECONCILE_WINDOW_DAYS`]): the
//! Redis hash carries a 48 h TTL, so a hash older than
//! yesterday is already gone (or about to be) — reconciling
//! it would resurrect a hash Redis deliberately expired, and
//! the DB row for that day is already the only copy. A day
//! inside the window is reconciled even when it is still in
//! progress (today): the CAS re-reads on conflict, so a
//! record landing mid-run is never lost from the durable
//! row — its hash increment can be transiently set back
//! (ADR-0048) and the next run repairs it — and tomorrow's
//! run catches whatever drifts after this one.
//!
//! # The candidate set
//!
//! Two legs, unioned — a drift that only one leg can see is
//! still drift:
//!
//! * the DB leg: every `(user, day)` with a durable row in
//!   the window (`usage_counters::user_days_since`);
//! * the Redis leg: a SCAN of `pf:usage:*:{day}` per window
//!   day, parsed back to `(user, day)`. This is the only leg
//!   that sees a Redis-only hash — the `record` failure mode
//!   (Redis written, DB write failed) leaves exactly that
//!   behind. DB-wins then means the hash is zeroed: the DB
//!   is the source of truth and a row the DB never got is
//!   not usage the quota decisions can see (ASSUMPTION-043).
//!
//! # Failure handling
//!
//! One user-day failing (a Redis WRONGTYPE, a lost CAS after
//! three retries) is logged + counted and the sweep
//! continues — the dunning precedent (P1-070). Only a
//! failure to LIST the candidates (the DB query or the SCAN)
//! fails the whole job: without the list there is no run to
//! report on.
//!
//! # Invocation
//!
//! [`run_reconciliation`] is the invocable job: the worker's
//! nightly schedule (02:00 UTC, `PF_WORKER_NIGHTLY_HOUR_UTC`)
//! enqueues the `reconciliation` kind, whose handler runs
//! this. `false` on `PF_BILLING_RECONCILE_ENABLED` pauses it
//! without touching the schedule (the dunning kill-switch
//! precedent) — the report says `ran: false` and every count
//! is 0.

use crate::audit::{record, NewAuditLog};
use crate::clock::Clock;
use crate::config::Settings;
use crate::db::DbPool;
use crate::redis::RedisPool;
use crate::repos::usage_counters;
use crate::usage::{UsageError, UsageMeter};
use chrono::NaiveDate;
use deadpool_redis::redis::cmd;
use std::collections::BTreeSet;
use std::sync::Arc;
use uuid::Uuid;

/// The audit action for a correction beyond the threshold.
pub const RECONCILE_AUDIT_ACTION: &str = "billing.usage.reconciled";

/// How many days the nightly run covers: today + yesterday.
/// The Redis hash TTL is 48 h, so yesterday is the oldest
/// day whose hash can still exist.
pub const RECONCILE_WINDOW_DAYS: u32 = 2;

/// The reconciliation policy, from `Settings` (ADR-0046).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconcileConfig {
    /// `PF_BILLING_RECONCILE_ENABLED` — the kill-switch.
    pub enabled: bool,
    /// `PF_BILLING_RECONCILE_AUDIT_THRESHOLD_IMAGES` — a
    /// correction whose images delta reaches this magnitude
    /// is audited. 0 audits every correction.
    pub audit_threshold_images: i64,
    /// `PF_BILLING_RECONCILE_AUDIT_THRESHOLD_VIDEO_SECONDS` —
    /// a correction whose video-seconds delta reaches this
    /// magnitude is audited. 0 audits every correction.
    pub audit_threshold_video_seconds: i64,
}

impl ReconcileConfig {
    /// The policy as configured (defaults: enabled, 10 / 60).
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            enabled: settings.reconcile_enabled,
            audit_threshold_images: settings.reconcile_audit_threshold_images,
            audit_threshold_video_seconds: settings.reconcile_audit_threshold_video_seconds,
        }
    }
}

/// One corrected user-day: the Redis values before the fix
/// and the DB values after it (DB wins). The deltas are
/// `db - redis_before` — positive when Redis was behind the
/// truth, negative when it was ahead of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Correction {
    /// The user whose counters drifted.
    pub user_id: Uuid,
    /// The UTC day reconciled.
    pub day: NaiveDate,
    /// What Redis held before the fix (absent reads as 0).
    pub redis_images_before: i64,
    /// What Redis held before the fix (absent reads as 0).
    pub redis_video_seconds_before: i64,
    /// The DB values (the truth) Redis was set to.
    pub db_images: i64,
    /// The DB values (the truth) Redis was set to.
    pub db_video_seconds: i64,
}

impl Correction {
    /// The images drift (`db - redis_before`).
    pub fn images_delta(&self) -> i64 {
        self.db_images - self.redis_images_before
    }

    /// The video-seconds drift (`db - redis_before`).
    pub fn video_seconds_delta(&self) -> i64 {
        self.db_video_seconds - self.redis_video_seconds_before
    }

    /// Whether this correction is an anomaly: either leg's
    /// drift reaches its threshold. The comparison is on the
    /// magnitude — Redis ahead of the DB is as anomalous as
    /// Redis behind it, and a zero threshold audits every
    /// correction (the operator asked for the full trail).
    pub fn is_anomaly(&self, config: &ReconcileConfig) -> bool {
        self.images_delta().abs() >= config.audit_threshold_images
            || self.video_seconds_delta().abs() >= config.audit_threshold_video_seconds
    }
}

/// What one nightly run did.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReconciliationReport {
    /// Whether the job actually ran (`false` = disabled by
    /// config; every count below is 0 then).
    pub ran: bool,
    /// The first day in the window (yesterday).
    pub window_start: NaiveDate,
    /// The last day in the window (today).
    pub window_end: NaiveDate,
    /// User-days compared.
    pub checked: usize,
    /// User-days whose Redis hash was corrected.
    pub corrected: usize,
    /// Corrections audited (anomalies — the audit rows are
    /// the durable anomaly report).
    pub audited: usize,
    /// User-days that errored (logged, counted, sweep
    /// continued) — including anomalies whose audit row
    /// failed to land after the correction itself did.
    pub errors: usize,
    /// The audited corrections (the anomaly report, in
    /// memory — the audit rows are the durable copy).
    pub anomalies: Vec<Correction>,
}

/// The window days for a run at `today`: yesterday, then
/// today — oldest first, so the report reads in day order.
pub fn window_days(today: NaiveDate) -> Vec<NaiveDate> {
    (0..RECONCILE_WINDOW_DAYS)
        .rev()
        .map(|back| today - chrono::Duration::days(i64::from(back)))
        .collect()
}

/// Parse a Redis usage key back to its (user, day):
/// `pf:usage:{uuid}:{YYYY-MM-DD}`. Anything else is `None`
/// — the SCAN pattern only matches this shape, but a
/// hand-made key must never panic the parse (or worse, be
/// reconciled as someone else's day).
pub fn parse_usage_key(key: &str) -> Option<(Uuid, NaiveDate)> {
    let rest = key.strip_prefix("pf:usage:")?;
    let (user, day) = rest.split_once(':')?;
    let user_id = Uuid::parse_str(user).ok()?;
    let day = NaiveDate::parse_from_str(day, "%Y-%m-%d").ok()?;
    Some((user_id, day))
}

/// The Redis leg of the candidate set: every (user, day)
/// with a day-hash for `day`, via SCAN (never KEYS — the
/// keyspace is shared with the queues and the rate limiter).
/// The pattern pins the day, so a hash for any other day is
/// never returned; the parse drops anything that is not a
/// usage key (a hand-made key matching the pattern).
async fn redis_user_days(
    redis: &RedisPool,
    day: NaiveDate,
) -> Result<BTreeSet<(Uuid, NaiveDate)>, UsageError> {
    let pattern = format!("pf:usage:*:{day}", day = day.format("%Y-%m-%d"));
    let mut found = BTreeSet::new();
    let mut cursor = "0".to_string();
    loop {
        let mut conn = redis.get().await?;
        let (next, keys): (String, Vec<String>) = cmd("SCAN")
            .arg(&cursor)
            .arg("MATCH")
            .arg(&pattern)
            .arg("COUNT")
            .arg(1000)
            .query_async(&mut conn)
            .await?;
        for key in keys {
            if let Some(pair) = parse_usage_key(&key) {
                found.insert(pair);
            }
        }
        cursor = next;
        if cursor == "0" {
            break;
        }
    }
    Ok(found)
}

/// Run one nightly reconciliation pass over the window
/// (today + yesterday). See the module docs for the window,
/// the candidate set, and the failure handling. A correction
/// beyond the audit thresholds writes one
/// `billing.usage.reconciled` audit row (actor: the
/// `reconciliation` system client — no user acts here).
pub async fn run_reconciliation(
    pool: &DbPool,
    redis: &RedisPool,
    clock: Arc<dyn Clock>,
    config: &ReconcileConfig,
) -> Result<ReconciliationReport, UsageError> {
    let today = clock.now().date_naive();
    let days = window_days(today);
    let window_start = days[0];
    let mut report = ReconciliationReport {
        ran: false,
        window_start,
        window_end: today,
        ..ReconciliationReport::default()
    };
    if !config.enabled {
        tracing::info!("reconciliation: disabled by PF_BILLING_RECONCILE_ENABLED — no-op");
        return Ok(report);
    }
    report.ran = true;
    let meter = UsageMeter::new(pool.clone(), redis.clone(), clock);
    // The candidate set: DB rows in the window UNION the
    // Redis hashes in the window. Either leg failing fails
    // the whole job (no list, no run).
    let mut candidates = BTreeSet::new();
    for (user_id, day) in usage_counters::user_days_since(pool, window_start).await? {
        candidates.insert((user_id, day));
    }
    for day in &days {
        for pair in redis_user_days(redis, *day).await? {
            candidates.insert(pair);
        }
    }
    for (user_id, day) in candidates {
        match meter.reconcile_day(user_id, day).await {
            Ok(outcome) => {
                report.checked += 1;
                if !outcome.corrected {
                    continue;
                }
                report.corrected += 1;
                let correction = Correction {
                    user_id,
                    day,
                    redis_images_before: outcome.redis_images_before,
                    redis_video_seconds_before: outcome.redis_video_seconds_before,
                    db_images: outcome.db_images,
                    db_video_seconds: outcome.db_video_seconds,
                };
                if !correction.is_anomaly(config) {
                    continue;
                }
                match record(
                    pool,
                    &NewAuditLog {
                        actor_user_id: None,
                        actor_client: Some("reconciliation".to_string()),
                        action: RECONCILE_AUDIT_ACTION.to_string(),
                        target_type: "user".to_string(),
                        target_id: Some(user_id.to_string()),
                        before: Some(serde_json::json!({
                            "day": day.to_string(),
                            "images": correction.redis_images_before,
                            "video_seconds": correction.redis_video_seconds_before,
                        })),
                        after: Some(serde_json::json!({
                            "day": day.to_string(),
                            "images": correction.db_images,
                            "video_seconds": correction.db_video_seconds,
                        })),
                        reason: Some(
                            "nightly reconciliation: redis drift beyond audit threshold"
                                .to_string(),
                        ),
                    },
                )
                .await
                {
                    Ok(_) => {
                        report.audited += 1;
                        report.anomalies.push(correction);
                    }
                    Err(err) => {
                        // The correction landed but its audit
                        // row did not: count the error loudly
                        // and keep sweeping — the next run
                        // re-reports it only if the hash drifts
                        // again, so a missing audit row is a
                        // real (if rare) loss of trail.
                        tracing::error!(
                            user_id = %user_id,
                            day = %day,
                            error = ?err,
                            "reconciliation: anomaly audit row failed to land"
                        );
                        report.errors += 1;
                    }
                }
            }
            Err(err) => {
                tracing::error!(
                    user_id = %user_id,
                    day = %day,
                    error = ?err,
                    "reconciliation: user-day failed, continuing the sweep"
                );
                report.errors += 1;
            }
        }
    }
    tracing::info!(
        window_start = %report.window_start,
        window_end = %report.window_end,
        checked = report.checked,
        corrected = report.corrected,
        audited = report.audited,
        errors = report.errors,
        "reconciliation: pass complete"
    );
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> ReconcileConfig {
        ReconcileConfig {
            enabled: true,
            audit_threshold_images: 10,
            audit_threshold_video_seconds: 60,
        }
    }

    fn correction(
        images_before: i64,
        video_before: i64,
        db_images: i64,
        db_video: i64,
    ) -> Correction {
        Correction {
            user_id: Uuid::nil(),
            day: NaiveDate::from_ymd_opt(2027, 3, 10).unwrap(),
            redis_images_before: images_before,
            redis_video_seconds_before: video_before,
            db_images,
            db_video_seconds: db_video,
        }
    }

    #[test]
    fn anomaly_thresholds_are_magnitudes_on_either_leg() {
        // Images at the threshold: anomaly, even with no
        // video drift at all.
        assert!(correction(0, 120, 10, 120).is_anomaly(&config()));
        // Just under the images threshold, just under the
        // video threshold: noise, not an anomaly.
        assert!(!correction(0, 120, 9, 179).is_anomaly(&config()));
        // Video at the threshold: anomaly.
        assert!(correction(5, 60, 5, 120).is_anomaly(&config()));
        // Redis AHEAD of the DB counts too — the magnitude
        // is what matters, not the direction.
        assert!(correction(20, 120, 5, 120).is_anomaly(&config()));
        assert!(correction(5, 300, 5, 120).is_anomaly(&config()));
    }

    #[test]
    fn zero_threshold_audits_every_correction() {
        let zero = ReconcileConfig {
            enabled: true,
            audit_threshold_images: 0,
            audit_threshold_video_seconds: 0,
        };
        // Even a one-image drift is an anomaly at 0.
        assert!(correction(5, 120, 6, 120).is_anomaly(&zero));
        // A correction with no drift at all is not a
        // correction — but is_anomaly only sees deltas, and
        // 0 >= 0: callers only ask about real corrections.
        assert!(correction(5, 120, 5, 120).is_anomaly(&zero));
    }

    #[test]
    fn window_is_yesterday_then_today() {
        let today = NaiveDate::from_ymd_opt(2026, 11, 19).unwrap();
        assert_eq!(
            window_days(today),
            vec![
                NaiveDate::from_ymd_opt(2026, 11, 18).unwrap(),
                NaiveDate::from_ymd_opt(2026, 11, 19).unwrap(),
            ]
        );
    }

    #[test]
    fn window_crosses_month_boundaries() {
        let today = NaiveDate::from_ymd_opt(2026, 12, 1).unwrap();
        assert_eq!(
            window_days(today),
            vec![
                NaiveDate::from_ymd_opt(2026, 11, 30).unwrap(),
                NaiveDate::from_ymd_opt(2026, 12, 1).unwrap(),
            ]
        );
    }

    #[test]
    fn parse_usage_key_round_trips_and_rejects_junk() {
        let user = Uuid::parse_str("01911111-2222-7333-8444-555555555555").unwrap();
        let day = NaiveDate::from_ymd_opt(2026, 11, 19).unwrap();
        assert_eq!(
            parse_usage_key("pf:usage:01911111-2222-7333-8444-555555555555:2026-11-19"),
            Some((user, day))
        );
        assert_eq!(
            parse_usage_key("pf:usage:01911111-2222-7333-8444-555555555555"),
            None
        );
        assert_eq!(parse_usage_key("pf:usage:not-a-uuid:2026-11-19"), None);
        assert_eq!(
            parse_usage_key("pf:usage:01911111-2222-7333-8444-555555555555:19-11-2026"),
            None
        );
        assert_eq!(parse_usage_key("pf:queue:pipeline:active"), None);
        assert_eq!(parse_usage_key(""), None);
    }

    #[test]
    fn config_defaults_come_from_settings() {
        let settings = Settings::default();
        let config = ReconcileConfig::from_settings(&settings);
        assert!(config.enabled);
        assert_eq!(config.audit_threshold_images, 10);
        assert_eq!(config.audit_threshold_video_seconds, 60);
    }
}
