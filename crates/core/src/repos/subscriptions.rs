//! Subscriptions repository (P1-005, E1.1). The profile
//! endpoint reads the current plan here; billing tickets own
//! creation/cancellation. SQL lives here, not in services.

use super::error::RepoError;
use chrono::{DateTime, Utc};
use uuid::Uuid;

/// A row of `subscriptions` (migration 20261112065140; the
/// dunning columns are migration 20261118043000, P1-070).
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
    /// When the row entered `past_due` (P1-070, ADR-0035): the
    /// dunning clock starts here, from the FIRST failure — a
    /// second failed invoice while already past due is a
    /// same-state no-op and never re-stamps it. `None` exactly
    /// when the row is not `past_due` (a DB CHECK enforces it).
    pub past_due_since: Option<DateTime<Utc>>,
    /// The last dunning step mailed (P1-070): 0 = the day-0
    /// mail (the move itself), 3 = the day-3 reminder, 7 =
    /// the day-7 reminder. The job sends a step only when the
    /// row's step is below it, so a re-run sends nothing twice.
    pub dunning_step: i16,
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
                cancel_at_period_end, past_due_since, dunning_step,
                created_at, updated_at
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

/// Load one subscription row by id (P1-064, FR-P-04): the state
/// machine's service loads the row, validates the move against
/// its current status, then applies the guarded update below.
/// `None` when the row does not exist — the service answers 404
/// `PF-2010`, never inventing a row.
pub async fn by_id(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    id: Uuid,
) -> Result<Option<Subscription>, RepoError> {
    sqlx::query_as!(
        Subscription,
        "SELECT id, user_id, plan_id, provider, provider_customer_id,
                provider_subscription_id, status, current_period_end,
                cancel_at_period_end, past_due_since, dunning_step,
                created_at, updated_at
         FROM subscriptions WHERE id = $1",
        id,
    )
    .fetch_optional(pool)
    .await
    .map_err(RepoError::from_sqlx)
}

/// Create a subscription row (P1-065, FR-P-04): the webhook
/// handler inserts the row a `checkout.session.completed`
/// event describes — the user bought a plan at the provider,
/// and this is our record of it. The caller resolves the
/// user, the plan, and the provider-reported status first;
/// this is the insert only. No uniqueness is enforced here
/// beyond the primary key: the handler checks
/// [`by_provider_id`] before inserting, and the webhook
/// ledger's UNIQUE event id guarantees one dispatch per
/// delivery, so a second insert for the same provider
/// subscription cannot arrive through the receiver.
/// Nine arguments, like the P1-064 state service it pairs
/// with (`apps/api/src/services/subscription_state.rs`):
/// a params struct would hide which column each value lands
/// in, and the call sites pass them in column order.
/// `past_due_since` is `Some` exactly when `status` is
/// `past_due` (a checkout whose provider-reported status is
/// already past due — the DB CHECK enforces the pair): the
/// caller stamps it from the app clock, never from `now()`.
#[allow(clippy::too_many_arguments)]
pub async fn create(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    user_id: Uuid,
    plan_id: Uuid,
    provider: &str,
    provider_customer_id: &str,
    provider_subscription_id: &str,
    status: &str,
    current_period_end: Option<DateTime<Utc>>,
    cancel_at_period_end: bool,
    past_due_since: Option<DateTime<Utc>>,
) -> Result<Subscription, RepoError> {
    sqlx::query_as!(
        Subscription,
        "INSERT INTO subscriptions
             (user_id, plan_id, provider, provider_customer_id,
              provider_subscription_id, status, current_period_end,
              cancel_at_period_end, past_due_since)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
         RETURNING id, user_id, plan_id, provider, provider_customer_id,
                   provider_subscription_id, status, current_period_end,
                   cancel_at_period_end, past_due_since, dunning_step,
                   created_at, updated_at",
        user_id,
        plan_id,
        provider,
        provider_customer_id,
        provider_subscription_id,
        status,
        current_period_end,
        cancel_at_period_end,
        past_due_since,
    )
    .fetch_one(pool)
    .await
    .map_err(RepoError::from_sqlx)
}

/// Load a subscription row by its provider identity (P1-064,
/// FR-P-04): the webhook handlers (P1-065) key off the
/// provider's subscription id, not ours — the event carries
/// `sub_...`, never our UUID. `None` when no row carries that
/// provider subscription yet (a `checkout.session.completed`
/// for a subscription the receiver has not recorded — P1-065
/// inserts it then).
pub async fn by_provider_id(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    provider: &str,
    provider_subscription_id: &str,
) -> Result<Option<Subscription>, RepoError> {
    sqlx::query_as!(
        Subscription,
        "SELECT id, user_id, plan_id, provider, provider_customer_id,
                provider_subscription_id, status, current_period_end,
                cancel_at_period_end, past_due_since, dunning_step,
                created_at, updated_at
         FROM subscriptions
         WHERE provider = $1 AND provider_subscription_id = $2",
        provider,
        provider_subscription_id,
    )
    .fetch_optional(pool)
    .await
    .map_err(RepoError::from_sqlx)
}

/// Move a row's status, guarded on its current status (P1-064,
/// FR-P-04): the `UPDATE ... WHERE id = $1 AND status = $from`
/// is the concurrency guard — two handlers racing the same row
/// cannot both apply their move; the loser sees `false` and
/// re-reads (the uploads machine, P0-057, uses the same shape).
/// Returns `true` when exactly one row moved. The caller runs
/// this inside its transaction, next to the audit + outbox
/// writes, so a status change never lands without its trail.
/// `current_period_end` / `cancel_at_period_end` are the
/// provider-reported values the caller carries (P1-065 passes
/// the event's; a caller with none passes the row's own).
/// `past_due_since` / `dunning_step` are the dunning state
/// (P1-070, ADR-0035) the caller carries: `Some(now)` + 0 on
/// a move INTO `past_due`, `None` + 0 on any move OUT of it,
/// the row's own values otherwise — the guarded write is the
/// one place the columns move, so the status and its dunning
/// state can never disagree (the DB CHECK enforces the pair).
#[allow(clippy::too_many_arguments)]
pub async fn set_status_guarded(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    id: Uuid,
    from_status: &str,
    to_status: &str,
    current_period_end: Option<DateTime<Utc>>,
    cancel_at_period_end: bool,
    past_due_since: Option<DateTime<Utc>>,
    dunning_step: i16,
) -> Result<bool, RepoError> {
    let done = sqlx::query!(
        "UPDATE subscriptions
         SET status = $2, current_period_end = $3,
             cancel_at_period_end = $4, past_due_since = $5,
             dunning_step = $6, updated_at = now()
         WHERE id = $1 AND status = $7",
        id,
        to_status,
        current_period_end,
        cancel_at_period_end,
        past_due_since,
        dunning_step,
        from_status,
    )
    .execute(pool)
    .await
    .map_err(RepoError::from_sqlx)?;
    Ok(done.rows_affected() == 1)
}

/// Every `past_due` row, oldest failure first (P1-070): the
/// dunning job's scan. The `past_due_since IS NOT NULL`
/// filter is defense in depth — the DB CHECK already
/// guarantees it — so a hypothetical violation is skipped,
/// never crashed on.
pub async fn past_due(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
) -> Result<Vec<Subscription>, RepoError> {
    sqlx::query_as!(
        Subscription,
        "SELECT id, user_id, plan_id, provider, provider_customer_id,
                provider_subscription_id, status, current_period_end,
                cancel_at_period_end, past_due_since, dunning_step,
                created_at, updated_at
         FROM subscriptions
         WHERE status = 'past_due' AND past_due_since IS NOT NULL
         ORDER BY past_due_since ASC",
    )
    .fetch_all(pool)
    .await
    .map_err(RepoError::from_sqlx)
}

/// Advance a row's dunning step, guarded on its current step
/// (P1-070): the job's exactly-once write — a reminder lands
/// in the same transaction as this bump, so a concurrent job
/// run (or a re-run) sees the new step and skips the mail.
/// Returns `true` when exactly one row moved; `false` when
/// another run already advanced it (the caller re-reads).
pub async fn set_dunning_step_guarded(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    id: Uuid,
    from_step: i16,
    to_step: i16,
) -> Result<bool, RepoError> {
    let done = sqlx::query!(
        "UPDATE subscriptions
         SET dunning_step = $2, updated_at = now()
         WHERE id = $1 AND status = 'past_due' AND dunning_step = $3",
        id,
        to_step,
        from_step,
    )
    .execute(pool)
    .await
    .map_err(RepoError::from_sqlx)?;
    Ok(done.rows_affected() == 1)
}

/// Move a row to another plan, guarded on it still granting
/// one (P1-069, FR-P-03): the downgrade applier's write — the
/// plan switch lands exactly when the scheduled change comes
/// due, in the same transaction that closes the change row.
/// Returns `true` when exactly one row moved; `false` when
/// the row no longer grants a plan (already `past_due` or
/// `canceled` — the change is moot, and the applier closes
/// it as `canceled` instead).
pub async fn set_plan_guarded(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    id: Uuid,
    plan_id: Uuid,
) -> Result<bool, RepoError> {
    let done = sqlx::query!(
        "UPDATE subscriptions
         SET plan_id = $2, updated_at = now()
         WHERE id = $1 AND status IN ('trialing', 'active')",
        id,
        plan_id,
    )
    .execute(pool)
    .await
    .map_err(RepoError::from_sqlx)?;
    Ok(done.rows_affected() == 1)
}
