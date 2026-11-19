//! Daily usage counters repository (P1-005, E1.1). The
//! profile endpoint reads today's row for the quota snapshot;
//! the metering ticket (P1-050) owns the increment +
//! write-through. SQL lives here, not in services.

use super::error::RepoError;
use chrono::{DateTime, NaiveDate, Utc};
use uuid::Uuid;

/// A row of `usage_counters` (migration 20261112065140).
#[derive(Debug, Clone, PartialEq)]
pub struct UsageCounter {
    pub user_id: Uuid,
    pub day: NaiveDate,
    pub images_used: i32,
    pub video_seconds_used: i32,
    pub updated_at: DateTime<Utc>,
}

/// Today's row for a user (`day` is a UTC date — the counters
/// reset at the UTC boundary, P1-050). `None` when the user
/// has used nothing today — the caller treats that as zeros,
/// it does not create the row (reads never write).
pub async fn for_user_day(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    user_id: Uuid,
    day: NaiveDate,
) -> Result<Option<UsageCounter>, RepoError> {
    sqlx::query_as!(
        UsageCounter,
        "SELECT user_id, day, images_used, video_seconds_used, updated_at
         FROM usage_counters WHERE user_id = $1 AND day = $2",
        user_id,
        day,
    )
    .fetch_optional(pool)
    .await
    .map_err(RepoError::from_sqlx)
}

/// Add to a user's counters for a UTC day (P1-050, the
/// write-through leg of metering): insert the row when the
/// user has used nothing that day, else add to the existing
/// row. One statement, so concurrent increments cannot lose
/// a count (the row lock serializes the upserts). Both
/// deltas must be >= 0 — the callers never decrement a
/// counter (a correction goes through reconciliation, which
/// overwrites, never through here).
pub async fn increment(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    user_id: Uuid,
    day: NaiveDate,
    images: i32,
    video_seconds: i32,
) -> Result<UsageCounter, RepoError> {
    debug_assert!(images >= 0 && video_seconds >= 0);
    sqlx::query_as!(
        UsageCounter,
        "INSERT INTO usage_counters (user_id, day, images_used, video_seconds_used)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (user_id, day) DO UPDATE SET
           images_used = usage_counters.images_used + EXCLUDED.images_used,
           video_seconds_used = usage_counters.video_seconds_used + EXCLUDED.video_seconds_used,
           updated_at = now()
         RETURNING user_id, day, images_used, video_seconds_used, updated_at",
        user_id,
        day,
        images,
        video_seconds,
    )
    .fetch_one(pool)
    .await
    .map_err(RepoError::from_sqlx)
}

/// Overwrite a user's counters for a UTC day (P1-050,
/// reconciliation): the DB row is the source of truth, so a
/// correction sets the row to the reconciled values instead
/// of adding to it. Inserts the row when none exists (a day
/// the user used but whose row is missing is a zero row that
/// drifted, not a missing day).
pub async fn overwrite(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    user_id: Uuid,
    day: NaiveDate,
    images: i32,
    video_seconds: i32,
) -> Result<UsageCounter, RepoError> {
    debug_assert!(images >= 0 && video_seconds >= 0);
    sqlx::query_as!(
        UsageCounter,
        "INSERT INTO usage_counters (user_id, day, images_used, video_seconds_used)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (user_id, day) DO UPDATE SET
           images_used = EXCLUDED.images_used,
           video_seconds_used = EXCLUDED.video_seconds_used,
           updated_at = now()
         RETURNING user_id, day, images_used, video_seconds_used, updated_at",
        user_id,
        day,
        images,
        video_seconds,
    )
    .fetch_one(pool)
    .await
    .map_err(RepoError::from_sqlx)
}

/// Adjust a user's counters for a UTC day by signed deltas
/// (P1-079, the completion true-up): a video that ran
/// shorter than its estimate refunds seconds, a longer one
/// charges the extra. Either delta may be negative — the
/// row is floored at 0 in SQL on BOTH paths, so the
/// `usage_counters` CHECK (`images_used >= 0`,
/// `video_seconds_used >= 0`) can never trip: a missing row
/// inserts the clamped values, an existing row adds the
/// delta and floors the sum. One statement, so concurrent
/// adjustments cannot lose a count (the row lock
/// serializes the upserts). The caller is responsible for
/// the Redis leg — see `UsageMeter::true_up`, which
/// computes the delta the DB actually applied and mirrors
/// exactly that.
pub async fn adjust(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    user_id: Uuid,
    day: NaiveDate,
    images: i32,
    video_seconds: i32,
) -> Result<UsageCounter, RepoError> {
    // The `VALUES` row is clamped (a missing row inserts
    // 0, never a negative — the CHECK forbids it), but the
    // `ON CONFLICT` leg MUST use the raw `$3`/`$4`
    // parameters, not `EXCLUDED`: `EXCLUDED` carries the
    // already-clamped insert row, so a negative delta
    // would read as 0 and the refund would silently vanish.
    sqlx::query_as!(
        UsageCounter,
        "INSERT INTO usage_counters (user_id, day, images_used, video_seconds_used)
         VALUES ($1, $2, GREATEST(0, $3), GREATEST(0, $4))
         ON CONFLICT (user_id, day) DO UPDATE SET
           images_used = GREATEST(0, usage_counters.images_used + $3),
           video_seconds_used = GREATEST(0, usage_counters.video_seconds_used + $4),
           updated_at = now()
         RETURNING user_id, day, images_used, video_seconds_used, updated_at",
        user_id,
        day,
        images,
        video_seconds,
    )
    .fetch_one(pool)
    .await
    .map_err(RepoError::from_sqlx)
}
