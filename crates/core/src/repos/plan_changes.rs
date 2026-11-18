//! Scheduled plan changes repository (P1-069, FR-P-03).
//! A downgrade is a durable promise, not a provider call:
//! the row says "at `effective_at` this subscription is on
//! `to_plan_id`", and the webhook handler's applier
//! (`apps/api/src/services/plan_changes.rs`) flips the
//! subscription row + this row in one transaction when the
//! period rolls past it. SQL lives here, not in services.

use super::error::RepoError;
use chrono::{DateTime, Utc};
use uuid::Uuid;

/// A row of `scheduled_plan_changes` (migration
/// 20261118003342). `status` is `scheduled` → `applied` (the
/// applier ran) or `scheduled` → `canceled` (the subscription
/// ended first, or the change was superseded); rows are never
/// deleted — the audit trail needs them.
#[derive(Debug, Clone, PartialEq)]
pub struct ScheduledPlanChange {
    pub id: Uuid,
    pub user_id: Uuid,
    pub subscription_id: Uuid,
    pub from_plan_id: Uuid,
    pub to_plan_id: Uuid,
    pub effective_at: DateTime<Utc>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Schedule a downgrade. Fails with
/// [`RepoError::UniqueViolation`] when the subscription
/// already has a pending change — the partial UNIQUE index
/// (`status = 'scheduled'`) is the guard, and the service
/// turns the violation into 409.
pub async fn insert(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    user_id: Uuid,
    subscription_id: Uuid,
    from_plan_id: Uuid,
    to_plan_id: Uuid,
    effective_at: DateTime<Utc>,
) -> Result<ScheduledPlanChange, RepoError> {
    sqlx::query_as!(
        ScheduledPlanChange,
        "INSERT INTO scheduled_plan_changes
             (user_id, subscription_id, from_plan_id, to_plan_id, effective_at)
         VALUES ($1, $2, $3, $4, $5)
         RETURNING id, user_id, subscription_id, from_plan_id, to_plan_id,
                   effective_at, status, created_at, updated_at",
        user_id,
        subscription_id,
        from_plan_id,
        to_plan_id,
        effective_at,
    )
    .fetch_one(pool)
    .await
    .map_err(RepoError::from_sqlx)
}

/// Load one change row by id (P1-069, FR-P-03): the applier
/// re-reads the row after losing a race, to report the
/// winner's outcome. `None` when the row does not exist
/// (cannot happen for a row just loaded — but the caller
/// handles it as an internal error rather than crashing).
pub async fn by_id(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    id: Uuid,
) -> Result<Option<ScheduledPlanChange>, RepoError> {
    sqlx::query_as!(
        ScheduledPlanChange,
        "SELECT id, user_id, subscription_id, from_plan_id, to_plan_id,
                effective_at, status, created_at, updated_at
         FROM scheduled_plan_changes WHERE id = $1",
        id,
    )
    .fetch_optional(pool)
    .await
    .map_err(RepoError::from_sqlx)
}

/// The subscription's pending change, if one is scheduled.
/// The partial UNIQUE index guarantees at most one — the
/// `ORDER BY ... LIMIT 1` is a belt under that guarantee.
/// `None` when nothing is pending (never scheduled, already
/// applied, or mooted).
pub async fn pending_for_subscription(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    subscription_id: Uuid,
) -> Result<Option<ScheduledPlanChange>, RepoError> {
    sqlx::query_as!(
        ScheduledPlanChange,
        "SELECT id, user_id, subscription_id, from_plan_id, to_plan_id,
                effective_at, status, created_at, updated_at
         FROM scheduled_plan_changes
         WHERE subscription_id = $1 AND status = 'scheduled'
         ORDER BY created_at DESC LIMIT 1",
        subscription_id,
    )
    .fetch_optional(pool)
    .await
    .map_err(RepoError::from_sqlx)
}

/// Close a pending change, guarded on its current status
/// (`scheduled` → `applied` / `canceled`): returns `true`
/// when exactly one row moved. The caller runs this inside
/// its transaction, next to the subscription-row write, so a
/// change never lands applied without the plan switch it
/// records. A `false` means a concurrent applier (or a
/// mooting write) already closed it — the caller re-reads
/// instead of applying twice.
pub async fn set_status_guarded(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    id: Uuid,
    to_status: &str,
) -> Result<bool, RepoError> {
    let done = sqlx::query!(
        "UPDATE scheduled_plan_changes
         SET status = $2, updated_at = now()
         WHERE id = $1 AND status = 'scheduled'",
        id,
        to_status,
    )
    .execute(pool)
    .await
    .map_err(RepoError::from_sqlx)?;
    Ok(done.rows_affected() == 1)
}
