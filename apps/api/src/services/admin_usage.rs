//! Admin usage view (P1-081, FR-P-07): `GET
//! /v1/admin/usage/{id}` (JSON) + `GET
//! /v1/admin/usage/{id}/export` (CSV).
//!
//! The view is the P1-080 usage read for an arbitrary user:
//! the same 30-day window ending today (today + 30 buckets,
//! oldest first, missing days zero-filled) from the durable
//! `usage_counters` rows, plus the all-time `totals` (the
//! SUM over every row the user has — the window shows
//! recent use, the totals answer "how much has this user
//! ever used?" for a billing dispute), plus today's per-key
//! breakdown (`by_key`, always present here — an admin sees
//! everything; the key prefixes are labels, not secrets,
//! ADR-0044, and the argument applies a fortiori to an
//! admin). The CSV is the window as one row per day
//! (`date,images,video_seconds`, oldest first) —
//! machine-clean, no totals row: a dispute tool sums the
//! rows itself, and a totals row would break a parser that
//! expects every row to be a date. The totals live in the
//! JSON view.
//!
//! A user who does not exist is 404 `PF-2010` (the admin
//! asked about a specific user, so a missing one is a
//! wrong id, not an empty answer); a user who exists but
//! used nothing gets zeros, not a 404 (the P1-080
//! precedent — a list endpoint answers "what do they
//! have?"). No audit row: a read creates nothing durable
//! (P1-046 — reads are not audited). No Redis read: the DB
//! is the source of truth, never the hot hash.

use detag_core::repos::{usage_counters, users};
use serde::Serialize;
use thiserror::Error;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    services::usage::{get_usage, DayUsage, KeyUsage, UsageError},
    AppState,
};

/// A user's all-time usage totals: the sums over every
/// `usage_counters` row the user has, not just the 30-day
/// window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
pub struct UsageTotals {
    /// Images used, all time.
    pub images: i64,
    /// Video seconds used, all time.
    pub video_seconds: i64,
}

/// The admin usage view: the P1-080 shape for an arbitrary
/// user, plus the all-time totals and — always — today's
/// per-key breakdown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct AdminUsageResponse {
    /// Whose usage this is.
    #[schema(value_type = String, example = "123e4567-e89b-42d3-a456-426614174109")]
    pub user_id: Uuid,
    /// Today's bucket (the last entry of `days`, repeated
    /// here for convenience).
    pub today: DayUsage,
    /// The 30 daily buckets ending today, oldest first.
    /// Days with no usage are zeros, not absent rows.
    pub days: Vec<DayUsage>,
    /// All-time totals (every `usage_counters` row, not
    /// just the window).
    pub totals: UsageTotals,
    /// Today's per-key breakdown — always present in the
    /// admin view (calling key, sibling keys, and the
    /// unattributed row for web / JWT-created jobs).
    /// Empty when the user created nothing today.
    pub by_key: Vec<KeyUsage>,
}

/// What the admin usage view can fail with. `NotFound` is
/// the unknown-user 404; the rest are infrastructure
/// failures — the handler answers 500 `PF-5000`.
#[derive(Debug, Error)]
pub enum AdminUsageError {
    /// No such user.
    #[error("no such user")]
    NotFound,
    /// A database read failed.
    #[error("usage read failed")]
    Db(#[from] detag_core::repos::RepoError),
    /// The usage read failed in a way that is not a
    /// database error — unreachable for the breakdown
    /// (it never names a key), so this is a 500 too.
    #[error("usage read failed")]
    Internal,
}

impl From<UsageError> for AdminUsageError {
    fn from(err: UsageError) -> Self {
        match err {
            UsageError::Db(err) => AdminUsageError::Db(err),
            UsageError::Internal => AdminUsageError::Internal,
        }
    }
}

/// Read `user_id`'s usage as an admin sees it: the P1-080
/// window + today's per-key breakdown + the all-time
/// totals. The user must exist — a missing user is 404,
/// not zeros.
pub async fn get_admin_usage(
    state: &AppState,
    user_id: Uuid,
) -> Result<AdminUsageResponse, AdminUsageError> {
    if users::by_id(&state.pool, user_id).await?.is_none() {
        return Err(AdminUsageError::NotFound);
    }
    let usage = get_usage(state, user_id, true).await?;
    let (images, video_seconds) = usage_counters::totals_for_user(&state.pool, user_id).await?;
    Ok(AdminUsageResponse {
        user_id,
        today: usage.today,
        days: usage.days,
        totals: UsageTotals {
            images,
            video_seconds,
        },
        // The breakdown was requested (`with_breakdown:
        // true`), so it is `Some` — `unwrap_or_default`
        // only guards the type, it cannot fire.
        by_key: usage.by_key.unwrap_or_default(),
    })
}

/// Render the 30-day window as CSV: one header row plus one
/// row per day, oldest first. The values are a date and two
/// integers by construction, so no field ever needs quoting
/// or escaping — the format is fixed, and a parser can
/// split on commas and newlines. A trailing newline ends
/// the last row (the CSV convention: every record,
/// including the last, is newline-terminated).
pub fn render_usage_csv(days: &[DayUsage]) -> String {
    let mut out = String::with_capacity(days.len() * 24 + 24);
    out.push_str("date,images,video_seconds\n");
    for day in days {
        out.push_str(&format!(
            "{},{},{}\n",
            day.date, day.images, day.video_seconds
        ));
    }
    out
}

/// Export `user_id`'s usage as CSV: the same 30-day window
/// the JSON view carries, rendered by
/// [`render_usage_csv`]. The user must exist — a missing
/// user is 404, not an empty file.
pub async fn export_usage_csv(state: &AppState, user_id: Uuid) -> Result<String, AdminUsageError> {
    if users::by_id(&state.pool, user_id).await?.is_none() {
        return Err(AdminUsageError::NotFound);
    }
    let usage = get_usage(state, user_id, false).await?;
    Ok(render_usage_csv(&usage.days))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn day_usage(date: NaiveDate, images: i64, video_seconds: i64) -> DayUsage {
        DayUsage {
            date,
            images,
            video_seconds,
        }
    }

    #[test]
    fn csv_renders_header_plus_one_row_per_day_oldest_first() {
        let days = vec![
            day_usage(NaiveDate::from_ymd_opt(2026, 10, 21).unwrap(), 1, 30),
            day_usage(NaiveDate::from_ymd_opt(2026, 10, 22).unwrap(), 0, 0),
            day_usage(NaiveDate::from_ymd_opt(2026, 11, 19).unwrap(), 5, 120),
        ];
        assert_eq!(
            render_usage_csv(&days),
            "date,images,video_seconds\n\
             2026-10-21,1,30\n\
             2026-10-22,0,0\n\
             2026-11-19,5,120\n",
            "header + one row per day, oldest first, trailing newline"
        );
    }

    #[test]
    fn csv_with_no_days_is_header_only() {
        assert_eq!(
            render_usage_csv(&[]),
            "date,images,video_seconds\n",
            "an empty window is still a parseable CSV"
        );
    }
}
