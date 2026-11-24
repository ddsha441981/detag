//! Quota warning emails (P1-085, FR-P-07): warn a user when
//! their daily usage crosses a configured share of their plan's
//! daily limit — 80% by default, 100% by default — with a
//! once/day cap per level.
//!
//! # How a warning is decided
//!
//! [`maybe_warn_quota`] takes the user's effective plan (the
//! caller resolves it — the same `effective_plan` the quota
//! gate uses) and the UTC day, reads the durable snapshot for
//! that day ([`entitlements::quota_snapshot_for_plan`] — the
//! DB row, never the Redis hash, so the warning agrees with
//! the quota gate by construction), and compares each
//! dimension's usage against its limit. A dimension with
//! limit 0 (Free has no video-seconds) is skipped — a 0 limit
//! is "not granted", not "0% used". The tripping dimension is
//! the one with the highest percent; the level is `Full` at or
//! above `full_pct`, `Warn` at or above `warn_pct`.
//!
//! # The once/day cap
//!
//! The cap lives in the outbox itself, not in a second table:
//! [`email_outbox::try_enqueue_quota_warning`] inserts with
//! `ON CONFLICT DO NOTHING` against the partial unique index
//! on (`template`, `payload->>'user_id'`, `payload->>'day'`) for
//! the two quota-warning templates (migrations 20261120021325 +
//! 20261120023438 — the second put the template in the key after
//! the ticket's own live test caught the first version blocking
//! the 100% mail behind the 80% one). The
//! insert IS the check, so two concurrent calls racing the
//! same threshold enqueue exactly one mail — no advisory lock,
//! no read-then-write race. The two levels are independent
//! caps: an 80% mail already sent does not block the 100%
//! mail later the same day, and tomorrow is a new day (the
//! `day` key changes), so the cap resets at the UTC boundary
//! with the counters.
//!
//! # No audit row
//!
//! A warning is a routine notification, not an anomaly — the
//! outbox row itself is the record (support reads it per
//! user + template, the same way the resend limit does), so
//! no `audit_logs` row is written. The dunning reminders
//! differ: they are part of a money-owed sequence, so they
//! audit.
//!
//! # Invocation
//!
//! No production caller exists yet: usage is recorded only by
//! tests today (job creation is the 501 stub, completion lands
//! in Phase 2), so nothing calls this after a record today.
//! The Phase 2 intake calls `maybe_warn_quota` after each
//! successful `record` (and after each `true_up`, which can
//! also push a day over a threshold) — the function takes the
//! day explicitly so the caller owns it, exactly like
//! `UsageMeter::true_up`. Until then the mechanism + the proof
//! live here, and the tests call it directly with seeded usage
//! rows.

use crate::services::entitlements::{quota_snapshot_for_plan, EntitlementError, QuotaSnapshot};
use chrono::{DateTime, NaiveDate, Utc};
use detag_core::{
    checksum::sha256_hex,
    config::Settings,
    db::DbPool,
    error::AppError,
    repos::{
        email_outbox::{self, NewOutboxMail},
        plans::Plan,
        users,
    },
};
use uuid::Uuid;

/// The 80% warning template.
pub const QUOTA_WARNING_80_TEMPLATE: &str = "quota-warning-80";
/// The 100% warning template.
pub const QUOTA_WARNING_100_TEMPLATE: &str = "quota-warning-100";

/// Which warning level a day's usage reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WarnLevel {
    /// At or above `warn_pct`, below `full_pct`.
    Warn,
    /// At or above `full_pct`.
    Full,
}

impl WarnLevel {
    /// The template this level enqueues.
    pub fn template(self) -> &'static str {
        match self {
            WarnLevel::Warn => QUOTA_WARNING_80_TEMPLATE,
            WarnLevel::Full => QUOTA_WARNING_100_TEMPLATE,
        }
    }
}

/// Which quota dimension tripped the warning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WarnDimension {
    /// The daily-images limit.
    Images,
    /// The daily video-seconds limit.
    VideoSeconds,
}

impl WarnDimension {
    /// The payload value (`images` / `video_seconds`) — also
    /// the key the cap index does NOT read (the index keys on
    /// user + day + template; the dimension is payload detail).
    pub fn as_str(self) -> &'static str {
        match self {
            WarnDimension::Images => "images",
            WarnDimension::VideoSeconds => "video_seconds",
        }
    }

    /// The human unit the template renders (`images` /
    /// `video-seconds`).
    pub fn unit(self) -> &'static str {
        match self {
            WarnDimension::Images => "images",
            WarnDimension::VideoSeconds => "video-seconds",
        }
    }
}

/// One tripped threshold: the level, the dimension that
/// tripped it, and the numbers the mail states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WarnTrip {
    /// The level reached.
    pub level: WarnLevel,
    /// The dimension with the highest percent used.
    pub dimension: WarnDimension,
    /// That dimension's usage for the day.
    pub used: i64,
    /// That dimension's daily limit.
    pub limit: i64,
    /// `used * 100 / limit` (integer percent; can exceed 100
    /// when usage is over the limit, e.g. after an admin
    /// grant — the mail states the true number).
    pub percent: u32,
}

/// The quota-warning policy, from `Settings` (ADR-0049).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuotaWarningConfig {
    /// `PF_BILLING_QUOTA_WARN_ENABLED` — the kill-switch.
    pub enabled: bool,
    /// `PF_BILLING_QUOTA_WARN_PCT` — first threshold, percent.
    pub warn_pct: u32,
    /// `PF_BILLING_QUOTA_WARN_FULL_PCT` — second threshold,
    /// percent.
    pub full_pct: u32,
}

impl QuotaWarningConfig {
    /// The policy as configured (defaults: enabled, 80/100).
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            enabled: settings.quota_warn_enabled,
            warn_pct: settings.quota_warn_pct,
            full_pct: settings.quota_warn_full_pct,
        }
    }
}

/// What [`maybe_warn_quota`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WarnOutcome {
    /// Disabled by config — no mail, no read.
    Disabled,
    /// Below the warn threshold — nothing to send.
    BelowThreshold,
    /// The warning mail was enqueued (the outbox row id).
    Sent {
        /// The template enqueued.
        template: &'static str,
        /// The enqueued outbox row.
        outbox_id: Uuid,
        /// The trip the mail states.
        trip: WarnTrip,
    },
    /// This user/day/level already has a warning row (sent or
    /// still queued) — the cap held, no second mail.
    AlreadyWarned {
        /// The template that already has a row.
        template: &'static str,
        /// The trip that would have mailed.
        trip: WarnTrip,
    },
}

/// Percent of `limit` that `used` is, in whole percent.
/// `limit` must be > 0 (callers skip zero-limit dimensions).
/// i128 math: a usage count times 100 cannot overflow it.
fn pct(used: i64, limit: i64) -> u32 {
    debug_assert!(limit > 0);
    ((used as i128 * 100) / limit as i128) as u32
}

/// The pure threshold decision (P1-085, unit-tested below):
/// the highest percent across the dimensions that have a
/// limit, and the level it reaches. A dimension with limit 0
/// is skipped — Free's 0 video-seconds limit means "no video
/// granted", not a dimension at 0% (or at infinite percent
/// once any video-second is used). Ties go to images (the
/// first dimension checked) so the trip is deterministic.
pub fn warn_level(snapshot: &QuotaSnapshot, config: &QuotaWarningConfig) -> Option<WarnTrip> {
    let mut best: Option<(u32, WarnDimension, i64, i64)> = None;
    for (used, limit, dimension) in [
        (
            snapshot.daily_images.used,
            snapshot.daily_images.limit,
            WarnDimension::Images,
        ),
        (
            snapshot.daily_video_seconds.used,
            snapshot.daily_video_seconds.limit,
            WarnDimension::VideoSeconds,
        ),
    ] {
        if limit <= 0 {
            continue;
        }
        let percent = pct(used, limit);
        if best.is_none_or(|(best_pct, _, _, _)| percent > best_pct) {
            best = Some((percent, dimension, used, limit));
        }
    }
    let (percent, dimension, used, limit) = best?;
    let level = if percent >= config.full_pct {
        WarnLevel::Full
    } else if percent >= config.warn_pct {
        WarnLevel::Warn
    } else {
        return None;
    };
    Some(WarnTrip {
        level,
        dimension,
        used,
        limit,
        percent,
    })
}

/// Maybe enqueue the quota-warning mail for one user on one
/// UTC day (P1-085). The caller owns the plan (resolved once
/// — the quota gate's `effective_plan`) and the day (the
/// record's day, not necessarily today — a true-up can push a
/// past day over a threshold, and the warning belongs to that
/// day's cap key). `now` is the caller's clock instant, and
/// the snapshot's grant expiry reads it (P1-107) — for a
/// past-day warning the grants live at `now` are what count,
/// which is the honest reading: the warning fires for the
/// limits in force now.
pub async fn maybe_warn_quota(
    pool: &DbPool,
    config: &QuotaWarningConfig,
    user_id: Uuid,
    plan: &Plan,
    day: NaiveDate,
    now: DateTime<Utc>,
) -> Result<WarnOutcome, AppError> {
    if !config.enabled {
        return Ok(WarnOutcome::Disabled);
    }
    let user = users::by_id(pool, user_id)
        .await
        .map_err(|_| AppError::Internal)?
        .ok_or(AppError::Internal)?;
    let snapshot = quota_snapshot_for_plan(pool, user_id, plan, day, now)
        .await
        .map_err(|err| match err {
            EntitlementError::Db(_) => AppError::DatabaseUnavailable,
            EntitlementError::MissingFreePlan => AppError::Internal,
        })?;
    let Some(trip) = warn_level(&snapshot, config) else {
        return Ok(WarnOutcome::BelowThreshold);
    };
    let template = trip.level.template();
    let enqueued = email_outbox::try_enqueue_quota_warning(
        pool,
        &NewOutboxMail {
            to_hash: sha256_hex(user.email.as_bytes()),
            template: template.to_string(),
            payload: serde_json::json!({
                "user_id": user_id,
                "day": day.format("%Y-%m-%d").to_string(),
                "dimension": trip.dimension.as_str(),
                // The templates render `{{ unit }}` — the drain
                // adds only `base_url` / `app_name`, so the
                // payload itself must carry it, or the mail fails
                // to render and parks unsent.
                "unit": trip.dimension.unit(),
                "used": trip.used,
                "limit": trip.limit,
                "percent": trip.percent,
                "plan_code": plan.code,
            }),
            scheduled_at: None,
        },
    )
    .await
    .map_err(|_| AppError::Internal)?;
    match enqueued {
        Some(outbox_id) => Ok(WarnOutcome::Sent {
            template,
            outbox_id,
            trip,
        }),
        None => Ok(WarnOutcome::AlreadyWarned { template, trip }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::entitlements::{QuotaBucket, QuotaSnapshot};

    fn snapshot(
        images_used: i64,
        images_limit: i64,
        video_used: i64,
        video_limit: i64,
    ) -> QuotaSnapshot {
        QuotaSnapshot {
            daily_images: QuotaBucket {
                limit: images_limit,
                used: images_used,
                remaining: (images_limit - images_used).max(0),
            },
            daily_video_seconds: QuotaBucket {
                limit: video_limit,
                used: video_used,
                remaining: (video_limit - video_used).max(0),
            },
        }
    }

    fn config() -> QuotaWarningConfig {
        QuotaWarningConfig {
            enabled: true,
            warn_pct: 80,
            full_pct: 100,
        }
    }

    #[test]
    fn below_warn_is_nothing() {
        assert_eq!(warn_level(&snapshot(7, 10, 0, 0), &config()), None);
    }

    #[test]
    fn at_warn_exactly_is_warn() {
        assert_eq!(
            warn_level(&snapshot(8, 10, 0, 0), &config()),
            Some(WarnTrip {
                level: WarnLevel::Warn,
                dimension: WarnDimension::Images,
                used: 8,
                limit: 10,
                percent: 80,
            })
        );
    }

    #[test]
    fn at_99_is_still_warn() {
        let trip = warn_level(&snapshot(99, 100, 0, 0), &config()).expect("99 trips warn");
        assert_eq!(trip.level, WarnLevel::Warn);
        assert_eq!(trip.percent, 99);
    }

    #[test]
    fn at_full_exactly_is_full() {
        let trip = warn_level(&snapshot(10, 10, 0, 0), &config()).expect("100 trips full");
        assert_eq!(trip.level, WarnLevel::Full);
        assert_eq!(trip.percent, 100);
        assert_eq!(trip.level.template(), QUOTA_WARNING_100_TEMPLATE);
    }

    #[test]
    fn over_the_limit_is_full_with_the_true_percent() {
        let trip = warn_level(&snapshot(12, 10, 0, 0), &config()).expect("120 trips full");
        assert_eq!(trip.level, WarnLevel::Full);
        assert_eq!(trip.percent, 120);
    }

    #[test]
    fn zero_limit_dimension_is_skipped() {
        // Free: video limit 0, video used 500 — the video leg
        // must not trip (0 is "not granted", not a percent).
        assert_eq!(warn_level(&snapshot(0, 10, 500, 0), &config()), None);
    }

    #[test]
    fn both_zero_limits_is_nothing() {
        assert_eq!(warn_level(&snapshot(5, 0, 500, 0), &config()), None);
    }

    #[test]
    fn video_dimension_trips_on_its_own() {
        let trip = warn_level(&snapshot(0, 10, 2880, 3600), &config()).expect("video trips");
        assert_eq!(trip.level, WarnLevel::Warn);
        assert_eq!(trip.dimension, WarnDimension::VideoSeconds);
        assert_eq!(trip.used, 2880);
        assert_eq!(trip.limit, 3600);
        assert_eq!(trip.percent, 80);
    }

    #[test]
    fn highest_percent_wins() {
        // Images 80%, video 95% — the video leg trips.
        let trip = warn_level(&snapshot(8, 10, 3420, 3600), &config()).expect("trips");
        assert_eq!(trip.dimension, WarnDimension::VideoSeconds);
        assert_eq!(trip.percent, 95);
        assert_eq!(trip.level, WarnLevel::Warn);
    }

    #[test]
    fn full_beats_warn_across_dimensions() {
        // Images 100% (full), video 80% (warn) — full wins.
        let trip = warn_level(&snapshot(10, 10, 2880, 3600), &config()).expect("trips");
        assert_eq!(trip.level, WarnLevel::Full);
        assert_eq!(trip.dimension, WarnDimension::Images);
    }

    #[test]
    fn custom_thresholds_follow_config() {
        let custom = QuotaWarningConfig {
            enabled: true,
            warn_pct: 50,
            full_pct: 90,
        };
        let trip = warn_level(&snapshot(5, 10, 0, 0), &custom).expect("50 trips warn");
        assert_eq!(trip.level, WarnLevel::Warn);
        let trip = warn_level(&snapshot(9, 10, 0, 0), &custom).expect("90 trips full");
        assert_eq!(trip.level, WarnLevel::Full);
        assert_eq!(warn_level(&snapshot(4, 10, 0, 0), &custom), None);
    }

    #[test]
    fn config_defaults_are_80_and_100() {
        let from = QuotaWarningConfig::from_settings(&Settings::default());
        assert!(from.enabled);
        assert_eq!(from.warn_pct, 80);
        assert_eq!(from.full_pct, 100);
    }
}
