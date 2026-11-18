//! Plans repository (P0-041, E0.4). Plans are seeded by migration
//! 003 (FR-P-01) and read here; admin edits go through the admin
//! API (P1-056), not this repo directly — the service in
//! `apps/api/src/services/admin_plans.rs` owns the edit flow
//! (validation + removal guards) and calls these writers inside
//! one transaction. `query_as!` macros are
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
    /// The Razorpay plan id (`plan_...`, P1-073, FR-P-03):
    /// `NULL` until the plan is created in the Razorpay
    /// dashboard and linked here — the Razorpay checkout
    /// sells this id, exactly as `stripe_price_id` is the
    /// Stripe side of the same link.
    pub razorpay_plan_id: Option<String>,
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
        "SELECT id, code, stripe_price_id, razorpay_plan_id, currency, amount, entitlements, created_at, updated_at
         FROM plans WHERE code = $1",
        code
    )
    .fetch_optional(pool)
    .await
    .map_err(RepoError::from_sqlx)
}

/// Fetch by id (P1-069, FR-P-03): the downgrade applier
/// resolves the scheduled change's target plan from its id.
/// `None` when the plan does not exist (cannot happen for a
/// scheduled change — `to_plan_id` is `ON DELETE RESTRICT` —
/// but the caller handles it as "no plan" rather than
/// crashing).
pub async fn by_id(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    id: Uuid,
) -> Result<Option<Plan>, RepoError> {
    sqlx::query_as!(
        Plan,
        "SELECT id, code, stripe_price_id, razorpay_plan_id, currency, amount, entitlements, created_at, updated_at
         FROM plans WHERE id = $1",
        id
    )
    .fetch_optional(pool)
    .await
    .map_err(RepoError::from_sqlx)
}

/// Fetch by Stripe price id (P1-065, FR-P-04): the webhook
/// handler resolves the plan a checkout bought from the price
/// the provider reports on the subscription — the price is the
/// provider's id for the plan, and this is the catalog side of
/// that link. `None` when no plan carries that price (a price
/// configured at Stripe but never synced into the catalog, or
/// a plan whose price changed after the checkout — the handler
/// logs it instead of inventing a plan).
pub async fn by_stripe_price_id(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    stripe_price_id: &str,
) -> Result<Option<Plan>, RepoError> {
    sqlx::query_as!(
        Plan,
        "SELECT id, code, stripe_price_id, razorpay_plan_id, currency, amount, entitlements, created_at, updated_at
         FROM plans WHERE stripe_price_id = $1",
        stripe_price_id
    )
    .fetch_optional(pool)
    .await
    .map_err(RepoError::from_sqlx)
}

/// Fetch by Razorpay plan id (P1-073, FR-P-03): the Razorpay
/// webhook handler resolves the plan a checkout bought from
/// the plan id Razorpay reports on the subscription — the
/// mirror of [`by_stripe_price_id`]. `None` when no plan
/// carries that id (a plan created at Razorpay but never
/// linked into the catalog — the handler logs it instead of
/// inventing a plan).
pub async fn by_razorpay_plan_id(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    razorpay_plan_id: &str,
) -> Result<Option<Plan>, RepoError> {
    sqlx::query_as!(
        Plan,
        "SELECT id, code, stripe_price_id, razorpay_plan_id, currency, amount, entitlements, created_at, updated_at
         FROM plans WHERE razorpay_plan_id = $1",
        razorpay_plan_id
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
        "SELECT id, code, stripe_price_id, razorpay_plan_id, currency, amount, entitlements, created_at, updated_at
         FROM plans ORDER BY amount"
    )
    .fetch_all(pool)
    .await
    .map_err(RepoError::from_sqlx)
}

/// Update a plan by code (P1-056, FR-P-01): the caller passes
/// the merged row — the service reads the current row first
/// and overlays only the fields the admin sent, so the repo
/// writes the full row. `None` when the code does not exist
/// (no row is written). `updated_at` is bumped by the write.
pub async fn update_by_code(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    code: &str,
    amount: i32,
    currency: &str,
    stripe_price_id: Option<&str>,
    entitlements: &serde_json::Value,
) -> Result<Option<Plan>, RepoError> {
    sqlx::query_as!(
        Plan,
        "UPDATE plans SET amount = $2, currency = $3, stripe_price_id = $4,
                entitlements = $5, updated_at = now()
         WHERE code = $1
         RETURNING id, code, stripe_price_id, razorpay_plan_id, currency, amount, entitlements,
                   created_at, updated_at",
        code,
        amount,
        currency,
        stripe_price_id,
        entitlements,
    )
    .fetch_optional(pool)
    .await
    .map_err(RepoError::from_sqlx)
}

/// Delete a plan by code (P1-056, FR-P-01). `true` when a row
/// was deleted, `false` when the code did not exist. The
/// caller guards the delete first: `subscriptions.plan_id`
/// is `ON DELETE RESTRICT`, so a plan with any subscription
/// row fails here with a foreign-key error — the service
/// counts subscriptions before calling this and answers 409
/// instead.
pub async fn delete_by_code(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    code: &str,
) -> Result<bool, RepoError> {
    let done = sqlx::query!("DELETE FROM plans WHERE code = $1", code)
        .execute(pool)
        .await
        .map_err(RepoError::from_sqlx)?;
    Ok(done.rows_affected() > 0)
}
