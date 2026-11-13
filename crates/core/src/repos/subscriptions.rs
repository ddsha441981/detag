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
/// status is `trialing` or `active` (a `past_due` subscription
/// still grants the plan until it cancels — the billing
/// tickets own that transition; `canceled` never counts).
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
