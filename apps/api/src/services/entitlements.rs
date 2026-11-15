//! Entitlements (P1-005, P1-049): the current plan + today's
//! quota snapshot for a user.
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
//!
//! P1-049: the plan rows come through the cached catalog
//! (`detag_core::plans::PlanCatalog`) and the entitlements
//! through its parsed struct (`detag_core::plans::
//! Entitlements`) — the jsonb is interpreted in exactly one
//! place, so the profile, the middleware, and the quota
//! snapshot can never disagree about what a plan grants.

use detag_core::db::DbPool;
use detag_core::plans::{Entitlements, PlanCache, PlanCatalog, PlanStore};
use detag_core::repos::{plans, subscriptions, usage_counters, RepoError};

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

/// Where the effective plan came from: a live subscription
/// overrode the default, or there was no live subscription
/// and the user is on Free.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanSource {
    Subscription,
    FreeDefault,
}

/// The user's effective plan (P1-049): the plan row, its
/// parsed entitlements, and where it came from. The quota
/// middleware (P1-051) and the profile both read this —
/// they never re-derive the plan from the subscription
/// themselves.
#[derive(Debug, Clone)]
pub struct ResolvedPlan {
    pub plan: plans::Plan,
    pub entitlements: Entitlements,
    pub source: PlanSource,
}

/// The user's effective plan: the newest live subscription's
/// plan (`trialing`/`active`), else Free. A `past_due`
/// subscription does NOT count (billing owns that
/// transition); a `canceled` one never counts. When several
/// live subscriptions exist, the newest wins
/// (`active_for_user` orders by `created_at DESC`). A live
/// subscription whose plan row is gone (impossible while
/// `plan_id` is `ON DELETE RESTRICT`, but defended anyway)
/// falls back to Free rather than erroring.
pub async fn effective_plan<S: PlanStore, C: PlanCache>(
    catalog: &PlanCatalog<S, C>,
    pool: &DbPool,
    user_id: uuid::Uuid,
) -> Result<ResolvedPlan, EntitlementError> {
    let (plan, source) = match subscriptions::active_for_user(pool, user_id).await? {
        Some(sub) => match catalog.by_id(sub.plan_id).await? {
            Some(plan) => (plan, PlanSource::Subscription),
            None => (
                catalog
                    .by_code("free")
                    .await?
                    .ok_or(EntitlementError::MissingFreePlan)?,
                PlanSource::FreeDefault,
            ),
        },
        None => (
            catalog
                .by_code("free")
                .await?
                .ok_or(EntitlementError::MissingFreePlan)?,
            PlanSource::FreeDefault,
        ),
    };
    let entitlements = Entitlements::from_value(&plan.entitlements);
    Ok(ResolvedPlan {
        plan,
        entitlements,
        source,
    })
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
    let entitlements = Entitlements::from_value(&plan.entitlements);
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
        daily_images: bucket(entitlements.daily_images, images_used),
        daily_video_seconds: bucket(entitlements.daily_video_seconds, video_used),
    })
}
