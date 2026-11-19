//! Usage API (P1-080, FR-P-07): `GET /v1/usage`.
//!
//! The caller's own metering, read from the durable
//! `usage_counters` rows (the DB is the source of truth —
//! never the Redis hot hash, which may drift until the
//! nightly reconciliation): today's bucket plus the
//! 30-day window ending today (30 buckets, oldest first,
//! `today` duplicated as the last bucket for convenience),
//! and — only when the caller authenticated with an API
//! key — the per-key breakdown for today
//! ([`usage_breakdown_for_user`]).
//!
//! The breakdown is key-caller-only by the ticket's spec
//! ("per-key breakdown when api key used"): a JWT caller
//! gets the buckets and no `by_key` field at all. The
//! breakdown is the FULL per-user breakdown (every key
//! that created a job today plus the unattributed row for
//! web / JWT-created jobs) — that is the shape
//! `usage_breakdown_for_user` returns, per the P1-038 /
//! P1-079 module docs — so a key caller also sees sibling
//! keys' display prefixes (ADR-0044: prefixes are labels,
//! not secrets — they ride the admin jobs list too — and
//! the key was minted by the owner it reports on).
//!
//! `video_seconds` in the breakdown is 0 until Phase 2
//! records usage at job create/completion (the
//! usage_attribution module doc): a job row carries no
//! duration, so the only honest per-key video number
//! today is the video-job count in `video_jobs` — a caller
//! that needs seconds must not mistake the count for
//! them. The buckets' `video_seconds` are real (they
//! read the counters P1-079's true-up maintains).
//!
//! A user who used nothing gets zeros, not a 404 (a list
//! endpoint answers "what do I have?" — for a user who
//! never used anything the honest answer is "none"). No
//! audit row: a read creates nothing durable.

use chrono::NaiveDate;
use detag_core::repos::usage_counters::{self, UsageCounter};
use serde::Serialize;
use thiserror::Error;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    services::usage_attribution::{usage_breakdown_for_user, UsageAttributionError},
    AppState,
};

/// How many daily buckets the response carries: the
/// 30-day window ending today (today included).
pub const USAGE_WINDOW_DAYS: i64 = 30;

/// One day's usage: the counters for one UTC date. A day
/// with no row reads as zeros — the row only exists once
/// the user has used something that day.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
pub struct DayUsage {
    /// The UTC date this bucket covers (`YYYY-MM-DD`).
    pub date: NaiveDate,
    /// Images used that day.
    pub images: i64,
    /// Video seconds used that day.
    pub video_seconds: i64,
}

/// One key's usage for today: the counts plus the
/// identity the caller shows. `key_id` is `None` for the
/// unattributed row (web / JWT-created jobs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct KeyUsage {
    /// The key, or `None` for unattributed jobs.
    #[schema(value_type = String, example = "123e4567-e89b-42d3-a456-426614174109")]
    pub key_id: Option<Uuid>,
    /// The key's display prefix (`pf_live_…`), or `None`
    /// for the unattributed row.
    pub key_prefix: Option<String>,
    /// Image jobs created today by the key.
    pub images: i64,
    /// Video jobs created today by the key.
    pub video_jobs: i64,
    /// Video seconds used today by the key — 0 until
    /// Phase 2 records usage at job create/completion
    /// (see the module docs); read `video_jobs` for the
    /// count, never this field for seconds.
    pub video_seconds: i64,
}

/// The usage response: today's bucket, the 30-day window,
/// and — only for an API-key caller — today's per-key
/// breakdown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct UsageResponse {
    /// Whose usage this is.
    #[schema(value_type = String, example = "123e4567-e89b-42d3-a456-426614174109")]
    pub user_id: Uuid,
    /// Today's bucket (the last entry of `days`, repeated
    /// here for convenience).
    pub today: DayUsage,
    /// The 30 daily buckets ending today, oldest first.
    /// Days with no usage are zeros, not absent rows.
    pub days: Vec<DayUsage>,
    /// Today's per-key breakdown — present only when the
    /// caller authenticated with an API key; absent for
    /// JWT callers. Empty when the caller created nothing
    /// today.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub by_key: Option<Vec<KeyUsage>>,
}

/// What reading usage can fail with. Both variants are
/// infrastructure failures — the handler answers 500
/// `PF-5000` either way.
#[derive(Debug, Error)]
pub enum UsageError {
    /// A database read failed.
    #[error("usage read failed")]
    Db(#[from] detag_core::repos::RepoError),
    /// The attribution read failed in a way that is not a
    /// database error — unreachable for the breakdown
    /// (it never names a key), so this is a 500 too.
    #[error("usage read failed")]
    Internal,
}

/// Read `user_id`'s usage: today + the 30-day window
/// ending today, from the durable `usage_counters` rows.
/// `with_breakdown` adds today's per-key breakdown
/// (the route passes `true` exactly when the caller
/// authenticated with an API key).
pub async fn get_usage(
    state: &AppState,
    user_id: Uuid,
    with_breakdown: bool,
) -> Result<UsageResponse, UsageError> {
    let today = state.clock.now().date_naive();
    let start = today - chrono::Duration::days(USAGE_WINDOW_DAYS - 1);
    let rows = usage_counters::for_user_range(&state.pool, user_id, start, today).await?;
    let days = fill_window(rows, start, today);
    let today_usage = *days.last().expect("the window always ends on today");
    let by_key = if with_breakdown {
        let breakdown = usage_breakdown_for_user(&state.pool, user_id, today)
            .await
            .map_err(|err| match err {
                UsageAttributionError::Db(err) => UsageError::Db(err),
                // The breakdown never names a key (it lists
                // the user's keys), so `UnknownKey` cannot
                // occur here — if it ever does, it is a 500.
                UsageAttributionError::UnknownKey => UsageError::Internal,
            })?;
        Some(
            breakdown
                .into_iter()
                .map(|key| KeyUsage {
                    key_id: key.key_id,
                    key_prefix: key.key_prefix,
                    images: key.images,
                    video_jobs: key.video_jobs,
                    video_seconds: key.video_seconds,
                })
                .collect(),
        )
    } else {
        None
    };
    Ok(UsageResponse {
        user_id,
        today: today_usage,
        days,
        by_key,
    })
}

/// Build the 30 daily buckets from the rows the range
/// query returned: one bucket per day from `start` to
/// `end` inclusive, oldest first; days with no row are
/// zeros. The query returns rows in day order, so a
/// single walk fills the window.
fn fill_window(rows: Vec<UsageCounter>, start: NaiveDate, end: NaiveDate) -> Vec<DayUsage> {
    let mut days = Vec::with_capacity(USAGE_WINDOW_DAYS as usize);
    let mut rows = rows.into_iter().peekable();
    let mut day = start;
    while day <= end {
        let (images, video_seconds) = match rows.peek() {
            Some(row) if row.day == day => {
                let row = rows.next().expect("peeked row exists");
                (
                    i64::from(row.images_used),
                    i64::from(row.video_seconds_used),
                )
            }
            _ => (0, 0),
        };
        days.push(DayUsage {
            date: day,
            images,
            video_seconds,
        });
        day += chrono::Duration::days(1);
    }
    days
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use chrono::Utc;

    fn row(day: NaiveDate, images: i32, video_seconds: i32) -> UsageCounter {
        UsageCounter {
            user_id: Uuid::now_v7(),
            day,
            images_used: images,
            video_seconds_used: video_seconds,
            updated_at: Utc.with_ymd_and_hms(2026, 11, 19, 12, 0, 0).unwrap(),
        }
    }

    #[test]
    fn window_fills_missing_days_with_zeros() {
        let start = NaiveDate::from_ymd_opt(2026, 10, 21).unwrap();
        let end = NaiveDate::from_ymd_opt(2026, 11, 19).unwrap();
        let days = fill_window(
            vec![
                row(NaiveDate::from_ymd_opt(2026, 10, 21).unwrap(), 1, 30),
                row(NaiveDate::from_ymd_opt(2026, 11, 19).unwrap(), 5, 120),
            ],
            start,
            end,
        );
        assert_eq!(days.len(), 30, "the window is always 30 buckets");
        assert_eq!(days[0].date, start);
        assert_eq!((days[0].images, days[0].video_seconds), (1, 30));
        assert_eq!(days[29].date, end);
        assert_eq!((days[29].images, days[29].video_seconds), (5, 120));
        // A middle day with no row is zeros, not absent.
        let middle = days
            .iter()
            .find(|day| day.date == NaiveDate::from_ymd_opt(2026, 11, 10).unwrap())
            .expect("every day in the window has a bucket");
        assert_eq!((middle.images, middle.video_seconds), (0, 0));
    }

    #[test]
    fn window_with_no_rows_is_all_zeros() {
        let start = NaiveDate::from_ymd_opt(2026, 10, 21).unwrap();
        let end = NaiveDate::from_ymd_opt(2026, 11, 19).unwrap();
        let days = fill_window(vec![], start, end);
        assert_eq!(days.len(), 30);
        assert!(days
            .iter()
            .all(|day| day.images == 0 && day.video_seconds == 0));
        assert_eq!(days[0].date, start);
        assert_eq!(days[29].date, end);
    }
}
