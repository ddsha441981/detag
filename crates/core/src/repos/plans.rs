//! Plans repository (P0-041, E0.4). Plans are seeded by migration
//! 003 (FR-P-01) and read here; admin edits go through the admin
//! API (Phase 4), not this repo. `query_as!` macros are
//! compile-time checked against the live schema; offline data is
//! checked in under `.sqlx/` (see `just sqlx-prepare`).

use super::error::RepoError;
use chrono::{DateTime, Utc};
use uuid::Uuid;

/// A row of `plans` (migration 003). `entitlements` is the raw jsonb
/// value (daily_images, daily_video_seconds, max_bytes,
/// result_ttl_hours, concurrency, lane, features); services
/// interpret its keys.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub id: Uuid,
    pub code: String,
    pub stripe_price_id: Option<String>,
    pub currency: String,
    pub amount: i32,
    pub entitlements: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Fetch by code (`free` / `pro` / `business`). `None` when the code
/// does not exist.
pub async fn by_code(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    code: &str,
) -> Result<Option<Plan>, RepoError> {
    sqlx::query_as!(
        Plan,
        "SELECT id, code, stripe_price_id, currency, amount, entitlements, created_at, updated_at
         FROM plans WHERE code = $1",
        code
    )
    .fetch_optional(pool)
    .await
    .map_err(RepoError::from_sqlx)
}

/// All plans, cheapest first (by `amount`).
pub async fn all(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
) -> Result<Vec<Plan>, RepoError> {
    sqlx::query_as!(
        Plan,
        "SELECT id, code, stripe_price_id, currency, amount, entitlements, created_at, updated_at
         FROM plans ORDER BY amount"
    )
    .fetch_all(pool)
    .await
    .map_err(RepoError::from_sqlx)
}
