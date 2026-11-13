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
