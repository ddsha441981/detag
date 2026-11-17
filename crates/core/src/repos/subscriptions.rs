//! Subscriptions repository (P1-005, E1.1). The profile
//! endpoint reads the current plan here; billing tickets own
//! creation/cancellation. SQL lives here, not in services.

use super::error::RepoError;
use chrono::{DateTime, Utc};
use uuid::Uuid;

/// A row of `subscriptions` (migration 20261112065140).
#[derive(Debug, Clone, PartialEq)]
pub struct Subscription {
    pub id: Uuid,
    pub user_id: Uuid,
    pub plan_id: Uuid,
    pub provider: String,
    pub provider_customer_id: Option<String>,
    pub provider_subscription_id: Option<String>,
    pub status: String,
    pub current_period_end: Option<DateTime<Utc>>,
    pub cancel_at_period_end: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// The user's current subscription: the newest row whose
/// status is `trialing` or `active`. A `past_due`
/// subscription does NOT count (it grants nothing until
/// billing moves it back to `active` — the billing tickets
/// own that transition); `canceled` never counts.
/// `None` when the user has no live subscription — the caller
/// falls back to the Free plan.
pub async fn active_for_user(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    user_id: Uuid,
) -> Result<Option<Subscription>, RepoError> {
    sqlx::query_as!(
        Subscription,
        "SELECT id, user_id, plan_id, provider, provider_customer_id,
                provider_subscription_id, status, current_period_end,
                cancel_at_period_end, created_at, updated_at
         FROM subscriptions
         WHERE user_id = $1 AND status IN ('trialing', 'active')
         ORDER BY created_at DESC LIMIT 1",
        user_id,
    )
    .fetch_optional(pool)
    .await
    .map_err(RepoError::from_sqlx)
}

/// Fetch a plan by id (the subscription's `plan_id`).
/// `None` when the plan does not exist (cannot happen for a
/// live subscription — `plan_id` is `ON DELETE RESTRICT` —
/// but the caller handles it as "no plan" rather than
/// crashing).
pub async fn plan_for_subscription(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    plan_id: Uuid,
) -> Result<Option<super::plans::Plan>, RepoError> {
    sqlx::query_as!(
        super::plans::Plan,
        "SELECT id, code, stripe_price_id, currency, amount, entitlements,
                created_at, updated_at
         FROM plans WHERE id = $1",
        plan_id,
    )
    .fetch_optional(pool)
    .await
    .map_err(RepoError::from_sqlx)
}

/// Live subscriptions on a plan (P1-056, FR-P-01): rows whose
/// status is `trialing` or `active` — the statuses that grant
/// the plan (P1-049). The admin editor refuses to remove a
/// plan while this is non-zero (409 `PF-2011`).
pub async fn count_live_for_plan(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    plan_id: Uuid,
) -> Result<i64, RepoError> {
    let row = sqlx::query!(
        "SELECT COUNT(*) AS \"count!\" FROM subscriptions \
         WHERE plan_id = $1 AND status IN ('trialing', 'active')",
        plan_id,
    )
    .fetch_one(pool)
    .await
    .map_err(RepoError::from_sqlx)?;
    Ok(row.count)
}

/// All subscriptions on a plan, live or historical (P1-056,
/// FR-P-01). The editor checks this too: `plan_id` is `ON
/// DELETE RESTRICT`, so even a plan whose subscriptions are
/// all `past_due` / `canceled` cannot be deleted at the DB
/// level — the service answers 409 for those as well, rather
/// than surfacing a foreign-key 500.
pub async fn count_all_for_plan(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    plan_id: Uuid,
) -> Result<i64, RepoError> {
    let row = sqlx::query!(
        "SELECT COUNT(*) AS \"count!\" FROM subscriptions WHERE plan_id = $1",
        plan_id,
    )
    .fetch_one(pool)
    .await
    .map_err(RepoError::from_sqlx)?;
    Ok(row.count)
}

/// The user's newest provider customer id, across ALL
/// subscription rows (P1-062, FR-P-03): the customer is a
/// provider-side identity that survives cancellation, so a
/// `canceled` row's customer is still the right one to reuse
/// at the next checkout — the newest row's id wins. `None`
/// when the user has no subscription row yet (first checkout:
/// the provider mints the customer at checkout and the
/// webhook records it on the subscription row, P1-063).
pub async fn latest_customer_id_for_user(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    user_id: Uuid,
) -> Result<Option<String>, RepoError> {
    let row = sqlx::query!(
        "SELECT provider_customer_id FROM subscriptions \
         WHERE user_id = $1 AND provider_customer_id IS NOT NULL \
         ORDER BY created_at DESC LIMIT 1",
        user_id,
    )
    .fetch_optional(pool)
    .await
    .map_err(RepoError::from_sqlx)?;
    Ok(row.and_then(|row| row.provider_customer_id))
}
