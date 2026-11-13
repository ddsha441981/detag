//! Entitlements (P1-005): the current plan + today's quota
//! snapshot for a user.
//!
//! This is THE function the quota middleware (P1-051) must
//! reuse — the profile snapshot and the middleware's pre-check
//! must agree, and they agree by construction when they read
//! the same two sources: the plan's entitlements (the
//! `plans` table, seeded per FR-P-01) and today's durable
//! usage row (`usage_counters`, UTC day). The middleware must
//! NOT re-derive the limits from anywhere else (not Redis —
//! the Redis mirrors are the hot path for metering, P1-050,
//! not the source of truth for the snapshot).
//!
//! The snapshot is a point-in-time read, not a transaction:
//! a plan change between the subscription read and the usage
//! read shows up on the next read. That is fine for a
//! snapshot (the middleware re-checks at job-create time).

use detag_core::db::DbPool;
use detag_core::repos::{plans, subscriptions, usage_counters, RepoError};
use serde_json::Value;

/// The plan, as the profile shows it: code + price. The
/// entitlements live in the quota snapshot (the limits), not
/// here — a client that wants the raw entitlements reads the
/// snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanSnapshot {
    pub code: String,
    pub amount: i32,
    pub currency: String,
}

/// One quota bucket: the daily limit, today's usage, and what
/// is left (`max(0, limit - used)` — remaining never goes
/// negative, even if usage overshot the limit).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuotaBucket {
    pub limit: i64,
    pub used: i64,
    pub remaining: i64,
}

fn bucket(limit: i64, used: i64) -> QuotaBucket {
    QuotaBucket {
        limit,
        used,
        remaining: (limit - used).max(0),
    }
}

/// Today's quota snapshot: the two daily buckets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuotaSnapshot {
    pub daily_images: QuotaBucket,
    pub daily_video_seconds: QuotaBucket,
}

/// What can go wrong reading entitlements. `Db` is a real
/// database failure (500); `MissingFreePlan` means the seed
/// never ran — the `free` plan is part of the billing
/// migration, so its absence means the database is broken,
/// not that the user has no plan.
#[derive(Debug, thiserror::Error)]
pub enum EntitlementError {
    #[error("entitlement read failed")]
    Db(#[from] RepoError),
    #[error("the free plan is missing from the database")]
    MissingFreePlan,
}

/// The user's current plan row: the live subscription's plan
/// (`trialing`/`active`), else Free. A `past_due`
/// subscription does NOT count (billing owns that
/// transition); a `canceled` one never counts.
pub async fn current_plan_row(
    pool: &DbPool,
    user_id: uuid::Uuid,
) -> Result<plans::Plan, EntitlementError> {
    let plan = match subscriptions::active_for_user(pool, user_id).await? {
        Some(sub) => subscriptions::plan_for_subscription(pool, sub.plan_id).await?,
        None => None,
    };
    match plan {
        Some(plan) => Ok(plan),
        None => plans::by_code(pool, "free")
            .await?
            .ok_or(EntitlementError::MissingFreePlan),
    }
}

/// The two daily limits from a plan's entitlements jsonb.
/// Missing/mistyped/negative keys read as 0 (fail closed).
fn limits_of(entitlements: &Value) -> (i64, i64) {
    let get = |key: &str| -> i64 {
        entitlements
            .get(key)
            .and_then(Value::as_i64)
            .unwrap_or(0)
            .max(0)
    };
    (get("daily_images"), get("daily_video_seconds"))
}

/// The plan snapshot for a plan the caller already loaded
/// (the profile service loads the plan once and reuses it for
/// both the plan section and the snapshot — this keeps the two
/// consistent within one response).
pub fn plan_snapshot_of(plan: &plans::Plan) -> PlanSnapshot {
    PlanSnapshot {
        code: plan.code.clone(),
        amount: plan.amount,
        currency: plan.currency.clone(),
    }
}

/// The quota snapshot for a plan the caller already loaded:
/// the plan's daily limits against today's durable usage row
/// (UTC day — the counters reset at the UTC boundary, P1-050).
/// No usage row today means zero usage. A missing or mistyped
/// entitlement key reads as limit 0 (fail closed: a plan
/// that does not grant a quota grants nothing).
pub async fn quota_snapshot_for_plan(
    pool: &DbPool,
    user_id: uuid::Uuid,
    plan: &plans::Plan,
) -> Result<QuotaSnapshot, EntitlementError> {
    let limits = limits_of(&plan.entitlements);
    let today = chrono::Utc::now().date_naive();
    let used = usage_counters::for_user_day(pool, user_id, today).await?;
    let (images_used, video_used) = match used {
        Some(row) => (
            i64::from(row.images_used),
            i64::from(row.video_seconds_used),
        ),
        None => (0, 0),
    };
    Ok(QuotaSnapshot {
        daily_images: bucket(limits.0, images_used),
        daily_video_seconds: bucket(limits.1, video_used),
    })
}
