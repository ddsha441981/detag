//! Usage metering (P1-050, FR-P-02 / FR-P-07).
//!
//! Two counters per user per UTC day — images and video
//! seconds — kept in two places that must agree:
//!
//! - **Redis (the hot path):** one hash per user-day,
//!   `pf:usage:{user}:{YYYY-MM-DD}` (see
//!   [`crate::redis::keys::usage_key`]), fields `images` /
//!   `video_seconds`, 48 h TTL ([`TTL_USAGE_COUNTER`]).
//!   Increments are one atomic Lua script (HINCRBY both
//!   fields + EXPIRE), so concurrent job-creates can never
//!   lose a count — the quota middleware (P1-051) reads
//!   this hash for its pre-check.
//! - **Postgres (the durable truth):** the
//!   `usage_counters` row for (user, UTC day), upserted on
//!   every record (the write-through leg). The DB row is
//!   the source of truth: when the two disagree, the DB
//!   wins.
//!
//! [`UsageMeter::record`] writes both, Redis first. A DB
//! failure after the Redis write leaves drift behind —
//! that is exactly what [`UsageMeter::reconcile_day`]
//! repairs (compare-and-set, so a concurrent record is
//! never wiped). The nightly job that runs the
//! reconciliation on a schedule is P1-082's; this ticket
//! delivers the mechanism + the proof it works.
//!
//! The day is always UTC, taken from the injected
//! [`Clock`] — never `Utc::now()` in the service — so the
//! UTC-boundary behaviour is testable with a frozen clock
//! (the ticket's AC).
//!
//! The video-seconds estimator ([`estimate_video_seconds`])
//! is the reservation at job-create time: the upload's
//! known duration, rounded up. An unknown duration
//! reserves nothing (ASSUMPTION-016) — the completion
//! true-up (P1-079) charges the real seconds.

use crate::clock::Clock;
use crate::db::DbPool;
use crate::redis::keys::{usage_key, TTL_USAGE_COUNTER};
use crate::redis::RedisPool;
use crate::repos::usage_counters::{self, UsageCounter};
use chrono::{DateTime, NaiveDate, Utc};
use deadpool_redis::redis::{cmd, Script};
use std::sync::Arc;
use thiserror::Error;
use uuid::Uuid;

/// The Lua sources (single source of truth — the `.lua`
/// files next to this module, same shape as the rate
/// limiter's scripts).
const USAGE_RECORD_LUA: &str = include_str!("usage_record.lua");
const USAGE_RECONCILE_LUA: &str = include_str!("usage_reconcile.lua");

/// How many times reconciliation retries its
/// compare-and-set before giving up (a record landing in
/// the same instant moves the hash; each retry re-reads).
const RECONCILE_ATTEMPTS: u32 = 3;

/// What can go wrong metering usage. `Db` / `Pool` /
/// `Redis` are transient infrastructure failures — the
/// caller decides whether to fail open or closed (the
/// quota middleware, P1-051, fails closed: a counter it
/// cannot read is not a counter it can trust).
/// `Conflict` means the reconcile CAS kept losing to
/// concurrent records — transient, retry on the next run.
#[derive(Debug, Error)]
pub enum UsageError {
    #[error("usage read/write failed")]
    Db(#[from] crate::repos::RepoError),
    #[error("redis pool error: {0}")]
    Pool(#[from] deadpool_redis::PoolError),
    #[error("redis error: {0}")]
    Redis(#[from] deadpool_redis::redis::RedisError),
    #[error("reconciliation conflict: the Redis hash moved under the read")]
    Conflict,
}

/// The outcome of one day's reconciliation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconcileOutcome {
    /// The UTC day reconciled.
    pub day: NaiveDate,
    /// The DB values (the truth).
    pub db_images: i64,
    pub db_video_seconds: i64,
    /// What Redis held before (absent fields read as 0).
    pub redis_images_before: i64,
    pub redis_video_seconds_before: i64,
    /// Whether Redis was corrected.
    pub corrected: bool,
}

/// The usage meter: Redis atomic counters + DB
/// write-through, on the injected clock's UTC day.
#[derive(Debug, Clone)]
pub struct UsageMeter {
    pool: DbPool,
    redis: RedisPool,
    clock: Arc<dyn Clock>,
}

impl UsageMeter {
    /// Build over a pool, a Redis pool, and a clock
    /// (`SystemClock` in production, `ManualClock` in
    /// tests).
    pub fn new(pool: DbPool, redis: RedisPool, clock: Arc<dyn Clock>) -> Self {
        UsageMeter { pool, redis, clock }
    }

    /// The UTC day the clock says it is (the counter day).
    pub fn today(&self) -> NaiveDate {
        self.clock.now().date_naive()
    }

    /// Record usage for a user on the clock's UTC day:
    /// atomically add to the Redis day-hash, then upsert
    /// the durable `usage_counters` row. Returns the DB
    /// row (the durable truth). Negative deltas are
    /// clamped to 0 — callers never decrement a counter
    /// (a correction goes through reconciliation).
    ///
    /// If the DB write fails after the Redis write, the
    /// error is returned and the drift stands — the
    /// nightly reconciliation repairs it. The Redis write
    /// is first so the enforcement source (what the quota
    /// middleware reads) is never behind the durable row.
    pub async fn record(
        &self,
        user_id: Uuid,
        images: i64,
        video_seconds: i64,
    ) -> Result<UsageCounter, UsageError> {
        let images = images.max(0);
        let video_seconds = video_seconds.max(0);
        let day = self.today();
        let key = usage_key(&user_id, day);
        let mut conn = self.redis.get().await?;
        let (_redis_images, _redis_video): (i64, i64) = Script::new(USAGE_RECORD_LUA)
            .key(&key)
            .arg(images)
            .arg(video_seconds)
            .arg(TTL_USAGE_COUNTER.as_secs() as i64)
            .invoke_async(&mut conn)
            .await?;
        let row = usage_counters::increment(
            &self.pool,
            user_id,
            day,
            i32::try_from(images).unwrap_or(i32::MAX),
            i32::try_from(video_seconds).unwrap_or(i32::MAX),
        )
        .await?;
        Ok(row)
    }

    /// Reconcile one user-day: the DB row is the source of
    /// truth, so a Redis hash that disagrees is overwritten
    /// with the DB values (compare-and-set, retried — a
    /// concurrent record moves the hash and the CAS fails
    /// rather than wiping the record). A missing DB row
    /// reads as zeros; a missing Redis hash reads as zeros.
    /// Returns whether Redis was corrected.
    pub async fn reconcile_day(
        &self,
        user_id: Uuid,
        day: NaiveDate,
    ) -> Result<ReconcileOutcome, UsageError> {
        let db_row = usage_counters::for_user_day(&self.pool, user_id, day).await?;
        let (db_images, db_video_seconds) = match db_row {
            Some(row) => (
                i64::from(row.images_used),
                i64::from(row.video_seconds_used),
            ),
            None => (0, 0),
        };
        let key = usage_key(&user_id, day);
        for _ in 0..RECONCILE_ATTEMPTS {
            let mut conn = self.redis.get().await?;
            let current: (Option<i64>, Option<i64>) = cmd("HMGET")
                .arg(&key)
                .arg("images")
                .arg("video_seconds")
                .query_async(&mut conn)
                .await?;
            let redis_before = (current.0.unwrap_or(0), current.1.unwrap_or(0));
            if redis_before == (db_images, db_video_seconds) {
                return Ok(ReconcileOutcome {
                    day,
                    db_images,
                    db_video_seconds,
                    redis_images_before: redis_before.0,
                    redis_video_seconds_before: redis_before.1,
                    corrected: false,
                });
            }
            // The CAS expects the exact fields read ('' when
            // a field is absent — an absent field is not a 0
            // to the script, even though both read as 0).
            let expected_images = current.0.map(|v| v.to_string()).unwrap_or_default();
            let expected_video = current.1.map(|v| v.to_string()).unwrap_or_default();
            let set: i32 = Script::new(USAGE_RECONCILE_LUA)
                .key(&key)
                .arg(expected_images)
                .arg(expected_video)
                .arg(db_images)
                .arg(db_video_seconds)
                .arg(TTL_USAGE_COUNTER.as_secs() as i64)
                .invoke_async(&mut conn)
                .await?;
            if set == 1 {
                return Ok(ReconcileOutcome {
                    day,
                    db_images,
                    db_video_seconds,
                    redis_images_before: redis_before.0,
                    redis_video_seconds_before: redis_before.1,
                    corrected: true,
                });
            }
            // The hash moved under the read — loop and
            // re-read; a record landing mid-reconcile is
            // never wiped.
        }
        Err(UsageError::Conflict)
    }
}

/// The video-seconds estimate at job-create time (P1-050):
/// the upload's known duration, rounded up to whole
/// seconds. An unknown, zero, negative, or non-finite
/// duration reserves nothing — there is no bitrate data
/// in the kit to invent an estimate from, and a made-up
/// number would be fake precision (ASSUMPTION-016). The
/// completion true-up (P1-079) charges the real seconds.
pub fn estimate_video_seconds(duration_s: Option<f32>) -> i64 {
    match duration_s {
        Some(seconds) if seconds.is_finite() && seconds > 0.0 => seconds.ceil() as i64,
        _ => 0,
    }
}

/// The UTC day for an instant (the counter day). Pure, so
/// the boundary rule is unit-testable without a meter.
pub fn day_of(when: DateTime<Utc>) -> NaiveDate {
    when.date_naive()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn estimator_rounds_known_durations_up() {
        assert_eq!(estimate_video_seconds(Some(90.0)), 90);
        // A fractional duration reserves the whole second
        // it touches — never less than the video runs.
        assert_eq!(estimate_video_seconds(Some(90.1)), 91);
        assert_eq!(estimate_video_seconds(Some(0.5)), 1);
    }

    #[test]
    fn estimator_reserves_nothing_without_a_duration() {
        assert_eq!(estimate_video_seconds(None), 0);
        assert_eq!(estimate_video_seconds(Some(0.0)), 0);
        assert_eq!(estimate_video_seconds(Some(-5.0)), 0);
        assert_eq!(estimate_video_seconds(Some(f32::NAN)), 0);
        assert_eq!(estimate_video_seconds(Some(f32::INFINITY)), 0);
    }

    #[test]
    fn day_is_utc_not_local() {
        // 23:59 UTC is still the 15th; one minute later it
        // is the 16th — the boundary is the UTC midnight,
        // and a local timezone never moves it.
        assert_eq!(
            day_of(Utc.with_ymd_and_hms(2026, 11, 15, 23, 59, 59).unwrap()),
            NaiveDate::from_ymd_opt(2026, 11, 15).unwrap()
        );
        assert_eq!(
            day_of(Utc.with_ymd_and_hms(2026, 11, 16, 0, 0, 0).unwrap()),
            NaiveDate::from_ymd_opt(2026, 11, 16).unwrap()
        );
    }
}
