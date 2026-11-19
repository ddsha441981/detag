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
//!   lose a count — the quota middleware (P1-051) reads the
//!   DURABLE snapshot for its pre-check, not this hash (the
//!   snapshot is the source the `/v1/me` quota agrees with
//!   by construction; see ASSUMPTION-017 — the hash is the
//!   metering hot path, the DB row is the decision source).
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
//! true-up ([`true_up_delta`] + [`UsageMeter::true_up`],
//! P1-079) charges the real seconds on the job's create
//! day, floored at 0 (ASSUMPTION-040).

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

/// The outcome of one completion true-up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrueUpOutcome {
    /// The UTC day the true-up landed on — the job's
    /// CREATE day, not necessarily today (see
    /// [`UsageMeter::true_up`]).
    pub day: NaiveDate,
    /// The seconds reserved at job create.
    pub estimated_seconds: i64,
    /// The seconds the video actually ran.
    pub actual_seconds: i64,
    /// The delta the DB actually applied (`actual -
    /// estimated`, floored so the day total never goes
    /// below 0). Redis receives exactly this delta.
    pub delta_applied: i64,
    /// The day's video-seconds total after the true-up
    /// (the durable row).
    pub video_seconds_used: i64,
    /// The day's video-seconds total in Redis after the
    /// true-up (the hot-path mirror).
    pub redis_video_seconds: i64,
}
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

    /// True-up one job's video seconds on completion
    /// (P1-079, FR-P-07): the estimate reserved at job
    /// create is replaced by the real duration. The delta
    /// is [`true_up_delta`] — positive when the video ran
    /// longer than estimated (charge the extra), negative
    /// when it ran shorter (refund the over-estimate).
    ///
    /// The true-up lands on `day` — the job's CREATE day,
    /// passed explicitly — NOT on the clock's today: a job
    /// that finishes after UTC midnight still corrects the
    /// day its estimate was reserved on, so the day's
    /// accounting stays whole (ASSUMPTION-040). The caller
    /// owns the day: it is the day `record` was called on
    /// at create time.
    ///
    /// Order is DB-first here, the reverse of `record` —
    /// deliberately: the DB decides how much of the delta
    /// actually applies (the day total floors at 0 — a
    /// refund can never drive a counter negative), and
    /// Redis then receives exactly the applied delta, so
    /// the two stores agree by construction. A Redis hash
    /// that already sits below the DB row (drift) is
    /// clamped the same way, so the hash never goes
    /// negative either; any residual drift is what
    /// `reconcile_day` repairs (DB wins). A Redis failure
    /// after the DB write leaves drift behind and returns
    /// the error — the same shape as `record`'s failure
    /// mode, in the other direction.
    pub async fn true_up(
        &self,
        user_id: Uuid,
        day: NaiveDate,
        estimated_seconds: i64,
        actual_seconds: i64,
    ) -> Result<TrueUpOutcome, UsageError> {
        let estimated_seconds = estimated_seconds.max(0);
        let actual_seconds = actual_seconds.max(0);
        let delta = true_up_delta(estimated_seconds, actual_seconds);
        // The DB decides the applied delta: read the day's
        // row first so the refund is floored at the current
        // total (a concurrent record landing between this
        // read and the upsert can shift Redis by that
        // record's amount at most — the DB row itself is
        // always exact, and reconcile repairs the hash).
        let current = usage_counters::for_user_day(&self.pool, user_id, day)
            .await?
            .map(|row| i64::from(row.video_seconds_used))
            .unwrap_or(0);
        let applied = (current + delta).max(0) - current;
        // The durable column is i32: cap the applied delta
        // at the headroom to `i32::MAX` so the DB add can
        // never overflow — and so the reported
        // `delta_applied` is the delta the DB actually
        // applied, not a larger wish.
        let applied = applied.min(i64::from(i32::MAX) - current);
        let row = usage_counters::adjust(
            &self.pool,
            user_id,
            day,
            0,
            i32::try_from(applied).unwrap_or(i32::MAX),
        )
        .await?;
        // Mirror exactly the applied delta into Redis —
        // never the raw delta, or a floored refund would
        // drive the hash below the DB row. Clamp against
        // the hash's own current value for the same reason:
        // a hash already behind the DB must not go negative.
        let key = usage_key(&user_id, day);
        let mut conn = self.redis.get().await?;
        let redis_before: (Option<i64>, Option<i64>) = cmd("HMGET")
            .arg(&key)
            .arg("images")
            .arg("video_seconds")
            .query_async(&mut conn)
            .await?;
        let redis_video = redis_before.1.unwrap_or(0);
        let redis_delta = applied.max(-redis_video);
        let (_redis_images, redis_video_seconds): (i64, i64) = Script::new(USAGE_RECORD_LUA)
            .key(&key)
            .arg(0)
            .arg(redis_delta)
            .arg(TTL_USAGE_COUNTER.as_secs() as i64)
            .invoke_async(&mut conn)
            .await?;
        Ok(TrueUpOutcome {
            day,
            estimated_seconds,
            actual_seconds,
            delta_applied: applied,
            video_seconds_used: i64::from(row.video_seconds_used),
            redis_video_seconds,
        })
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
/// completion true-up (P1-079, [`UsageMeter::true_up`])
/// charges the real seconds.
pub fn estimate_video_seconds(duration_s: Option<f32>) -> i64 {
    match duration_s {
        Some(seconds) if seconds.is_finite() && seconds > 0.0 => seconds.ceil() as i64,
        _ => 0,
    }
}

/// The completion true-up delta (P1-079, the ticket's AC):
/// the seconds a finished job still owes after its
/// estimate — `actual - estimated`. Positive when the
/// video ran longer than estimated (charge the extra),
/// negative when it ran shorter (refund the over-estimate),
/// zero when the estimate was exact or when nothing was
/// estimated and nothing ran. Negative inputs are clamped
/// to 0 first (a duration is never negative); the
/// subtraction saturates, so no input pair can overflow.
/// The delta is a *difference*, not a new total: the
/// caller floors the day total at 0 (see
/// [`UsageMeter::true_up`]) — a refund larger than the
/// day's recorded seconds applies only up to the floor.
pub fn true_up_delta(estimated_seconds: i64, actual_seconds: i64) -> i64 {
    actual_seconds
        .max(0)
        .saturating_sub(estimated_seconds.max(0))
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
    fn true_up_delta_is_actual_minus_estimate() {
        // Ran longer: charge the extra.
        assert_eq!(true_up_delta(90, 150), 60);
        // Ran shorter: refund the over-estimate.
        assert_eq!(true_up_delta(90, 30), -60);
        // Exact estimate: nothing owed.
        assert_eq!(true_up_delta(90, 90), 0);
        // Nothing estimated, nothing ran: nothing owed.
        assert_eq!(true_up_delta(0, 0), 0);
        // Unknown duration at create (estimate 0), real
        // seconds at completion: charge them all.
        assert_eq!(true_up_delta(0, 45), 45);
        // Ran zero seconds after an estimate: full refund.
        assert_eq!(true_up_delta(90, 0), -90);
    }

    #[test]
    fn true_up_delta_clamps_negative_inputs_to_zero() {
        // A negative estimate is nonsense input, not a
        // credit: it reserves 0.
        assert_eq!(true_up_delta(-5, 30), 30);
        // A negative actual is nonsense input, not a
        // negative duration: it ran 0.
        assert_eq!(true_up_delta(90, -5), -90);
        assert_eq!(true_up_delta(-5, -5), 0);
    }

    #[test]
    fn true_up_delta_saturates_instead_of_overflowing() {
        assert_eq!(true_up_delta(0, i64::MAX), i64::MAX);
        assert_eq!(true_up_delta(i64::MAX, 0), -i64::MAX);
        assert_eq!(true_up_delta(i64::MIN, i64::MAX), i64::MAX);
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
