//! Usage attribution (P1-038, FR-A-06): which API key a job's
//! usage belongs to.
//!
//! The attribution source is `jobs.key_id` (migration
//! 20261114180600): a job created through an API key records
//! the key; a web / JWT-created job records NULL. These
//! functions are the query layer the usage endpoints consume
//! — P1-080's `GET /v1/usage` per-key breakdown reads
//! [`usage_breakdown_for_user`], and the admin jobs list
//! (`GET /v1/admin/jobs`) reads the key prefix straight from
//! the jobs repo. Nothing here writes: attribution is derived
//! from the jobs table, never from a second counter that could
//! drift from it (the write-through counters land in P1-079).
//!
//! Video seconds are NOT here yet: a job row carries no
//! duration, so the only honest per-key video number today is
//! the video-job count. P1-079's estimator (video-seconds at
//! job create, true-up on completion) supplies the seconds;
//! until then `video_seconds` reads 0 and the count is in
//! `video_jobs` — a caller that needs seconds must not
//! mistake the count for them.

use detag_core::{
    db::DbPool,
    repos::{jobs, RepoError},
};
use uuid::Uuid;

/// One key's usage for one UTC day: the counts plus the
/// identity the caller shows. `key_id` is `None` for the
/// unattributed row (web / JWT-created jobs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyUsage {
    /// The key, or `None` for unattributed jobs.
    pub key_id: Option<Uuid>,
    /// The key's display prefix (`pf_live_…`), or `None` for
    /// the unattributed row.
    pub key_prefix: Option<String>,
    /// Image jobs created that day by the key.
    pub images: i64,
    /// Video jobs created that day by the key.
    pub video_jobs: i64,
    /// Video seconds used that day by the key — 0 until
    /// P1-079's estimator lands (see the module docs).
    pub video_seconds: i64,
}

/// What attribution reads can fail with. `Db` is a real
/// database failure (500); `UnknownKey` is a `key_id` with no
/// live row — the caller asked to attribute usage to a key
/// that does not exist (404 `PF-2010`, the api-keys shape).
#[derive(Debug, thiserror::Error)]
pub enum UsageAttributionError {
    #[error("usage attribution read failed")]
    Db(#[from] RepoError),
    #[error("no such api key")]
    UnknownKey,
}

impl From<jobs::KeyUsageRow> for KeyUsage {
    fn from(row: jobs::KeyUsageRow) -> Self {
        Self {
            key_id: row.key_id,
            key_prefix: row.key_prefix,
            images: row.images,
            video_jobs: row.video_jobs,
            video_seconds: 0,
        }
    }
}

/// One key's usage for one UTC day. The key must exist (a
/// `key_id` with no row is [`UsageAttributionError::UnknownKey`]
/// — attributing usage to a deleted key is a caller bug, not
/// an empty result). The prefix comes from the live `api_keys`
/// row, so a rotated key's usage keeps its current prefix.
pub async fn usage_for_key(
    pool: &DbPool,
    key_id: Uuid,
    day: chrono::NaiveDate,
) -> Result<KeyUsage, UsageAttributionError> {
    let key = detag_core::repos::api_keys::by_id(pool, key_id)
        .await?
        .ok_or(UsageAttributionError::UnknownKey)?;
    let usage = jobs::usage_by_key(pool, key_id, day).await?;
    Ok(KeyUsage {
        key_id: Some(key.id),
        key_prefix: Some(key.prefix),
        images: usage.images,
        video_jobs: usage.video_jobs,
        video_seconds: 0,
    })
}

/// A user's per-key usage breakdown for one UTC day: one row
/// per key that created a job that day, plus the unattributed
/// row when a web / JWT-created job exists. Empty when the
/// user created nothing that day.
pub async fn usage_breakdown_for_user(
    pool: &DbPool,
    user_id: Uuid,
    day: chrono::NaiveDate,
) -> Result<Vec<KeyUsage>, UsageAttributionError> {
    let rows = jobs::usage_breakdown_for_user(pool, user_id, day).await?;
    Ok(rows.into_iter().map(KeyUsage::from).collect())
}
