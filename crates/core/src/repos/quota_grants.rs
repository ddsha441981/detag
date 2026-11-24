//! Manual quota grants repository (P1-107). `quota_grants` is
//! the admin-granted, expiring bump to a user's daily quota
//! limits (migration 20261124080000): one row per grant —
//! `images` / `video_seconds` (at least one present, each
//! positive when present), `expires_at`, the `reason`, and
//! who granted it. Rows are never updated or deleted: an
//! expired grant stays as history and simply stops adding to
//! the limits. SQL lives here, not in services.

use super::error::RepoError;
use chrono::{DateTime, Utc};
use uuid::Uuid;

/// One quota grant (migration 20261124080000).
#[derive(Debug, Clone, PartialEq)]
pub struct QuotaGrant {
    pub id: Uuid,
    pub user_id: Uuid,
    pub images: Option<i32>,
    pub video_seconds: Option<i32>,
    pub expires_at: DateTime<Utc>,
    pub reason: String,
    pub granted_by: Uuid,
    pub created_at: DateTime<Utc>,
}

/// The values for a new grant. `expires_at` is computed by the
/// caller (the grant time + `ttl_hours` on the app clock), so
/// a frozen clock moves expiry too.
pub struct NewQuotaGrant {
    pub user_id: Uuid,
    pub images: Option<i32>,
    pub video_seconds: Option<i32>,
    pub expires_at: DateTime<Utc>,
    pub reason: String,
    pub granted_by: Uuid,
}

/// Insert one grant row, returning it. The caller writes the
/// audit row in the same transaction (the service's job, not
/// this repo's) — pass `&mut *conn` inside `tx::with_tx`.
pub async fn insert(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    grant: &NewQuotaGrant,
) -> Result<QuotaGrant, RepoError> {
    sqlx::query_as!(
        QuotaGrant,
        r#"INSERT INTO quota_grants (user_id, images, video_seconds, expires_at, reason, granted_by)
           VALUES ($1, $2, $3, $4, $5, $6)
           RETURNING id, user_id, images, video_seconds, expires_at, reason,
                     granted_by, created_at"#,
        grant.user_id,
        grant.images,
        grant.video_seconds,
        grant.expires_at,
        grant.reason,
        grant.granted_by,
    )
    .fetch_one(pool)
    .await
    .map_err(RepoError::from_sqlx)
}

/// The user's unexpired grants, summed: `(images, video_seconds)`
/// still live at `now`. Grants stack — two live grants add up —
/// and an expired grant contributes nothing (it stays in the
/// table as history). `COALESCE` keeps a user with no live
/// grants at (0, 0) instead of NULLs; `SUM` over `int` widens
/// to `bigint`, so the cast back to `i64` cannot overflow a
/// realistic grant (each grant is `i32`-bounded).
pub async fn unexpired_totals(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    user_id: Uuid,
    now: DateTime<Utc>,
) -> Result<(i64, i64), RepoError> {
    let row = sqlx::query!(
        r#"SELECT COALESCE(SUM(images), 0)::bigint AS "images_total!",
                  COALESCE(SUM(video_seconds), 0)::bigint AS "video_total!"
           FROM quota_grants
           WHERE user_id = $1 AND expires_at > $2"#,
        user_id,
        now,
    )
    .fetch_one(pool)
    .await
    .map_err(RepoError::from_sqlx)?;
    Ok((row.images_total, row.video_total))
}
