//! P1-049: the plans service — the catalog from the DB
//! (seeded) + cache, and effective-plan resolution
//! (subscription override).
//!
//! The catalog and the resolution are read through the live
//! harness DB (the seeded plans are the FR-P-01 rows), so the
//! tests prove the real seed values, not a fixture copy. The
//! six tests:
//!
//! - `catalog_lists_seeded_plans_cheapest_first` — the
//!   catalog holds exactly the three seeded plans, cheapest
//!   first, with the FR-P-01 amounts and all seven
//!   entitlement fields per plan.
//! - `effective_plan_defaults_to_free` — a user with no
//!   subscription resolves to Free (`FreeDefault`), with
//!   Free's entitlements.
//! - `effective_plan_trialing_subscription_overrides` — a
//!   `trialing` subscription to Pro resolves to Pro.
//! - `effective_plan_active_subscription_overrides` — an
//!   `active` subscription to Business resolves to Business.
//! - `effective_plan_past_due_and_canceled_do_not_count` —
//!   `past_due` and `canceled` subscriptions grant nothing:
//!   the user stays on Free.
//! - `effective_plan_newest_live_subscription_wins` — two
//!   live subscriptions: the newest one wins.
//! - `effective_plan_missing_free_plan_is_an_error` — a
//!   catalog with no `free` row errors `MissingFreePlan`
//!   instead of inventing a plan (a store that returns no
//!   rows stands in for a database whose seed never ran;
//!   the shared harness DB is never touched — deleting the
//!   seeded Free plan would break every other suite).

mod common;

use chrono::{DateTime, TimeDelta, Utc};
use common::{spawn_app, TestApp};
use detag_api::services::entitlements::{effective_plan, EntitlementError, PlanSource};
use detag_core::plans::{Entitlements, InMemoryPlanCache, PlanCatalog, PlanStore};
use detag_core::repos::{plans, RepoError};
use uuid::Uuid;

/// Insert a subscription row directly — there is no billing
/// flow yet to create one through the API. `created_at` is
/// explicit so the "newest wins" test controls the order
/// deterministically instead of relying on insert timing.
async fn seed_subscription(
    app: &TestApp,
    user_id: Uuid,
    plan_code: &str,
    status: &str,
    created_at: DateTime<Utc>,
) {
    let plan = plans::by_code(&app.pool, plan_code)
        .await
        .expect("load plan")
        .expect("plan is seeded");
    sqlx::query(
        "INSERT INTO subscriptions (user_id, plan_id, provider, status, created_at) \
         VALUES ($1, $2, 'stripe', $3, $4)",
    )
    .bind(user_id)
    .bind(plan.id)
    .bind(status)
    .bind(created_at)
    .execute(&app.pool)
    .await
    .expect("seed subscription");
}

#[tokio::test]
async fn catalog_lists_seeded_plans_cheapest_first() {
    let app = spawn_app().await;
    let catalog = app.state.plans.catalog().await.expect("catalog reads");
    assert_eq!(catalog.len(), 3, "exactly the three seeded plans");
    let codes: Vec<&str> = catalog.iter().map(|plan| plan.code.as_str()).collect();
    assert_eq!(codes, vec!["free", "pro", "business"], "cheapest first");

    let free = &catalog[0];
    assert_eq!(free.amount, 0);
    assert_eq!(free.currency, "INR");
    let ent = Entitlements::from_value(&free.entitlements);
    assert_eq!(ent.daily_images, 10);
    assert_eq!(ent.daily_video_seconds, 0);
    assert_eq!(ent.max_bytes, 26_214_400);
    assert_eq!(ent.result_ttl_hours, 6);
    assert_eq!(ent.concurrency, 1);
    assert!(!ent.has_feature("api_keys"));

    let pro = &catalog[1];
    assert_eq!(pro.amount, 49900);
    let ent = Entitlements::from_value(&pro.entitlements);
    assert_eq!(ent.daily_images, 500);
    assert_eq!(ent.daily_video_seconds, 3600);
    assert_eq!(ent.max_bytes, 1_073_741_824);
    assert_eq!(ent.result_ttl_hours, 24);
    assert_eq!(ent.concurrency, 3);
    assert!(ent.has_feature("api_keys"));
    assert!(ent.has_feature("webhooks"));
    assert!(!ent.has_feature("batch"));

    let business = &catalog[2];
    assert_eq!(business.amount, 199900);
    let ent = Entitlements::from_value(&business.entitlements);
    assert_eq!(ent.daily_images, 5000);
    assert_eq!(ent.daily_video_seconds, 18000);
    assert_eq!(ent.max_bytes, 4_294_967_296);
    assert_eq!(ent.result_ttl_hours, 72);
    assert_eq!(ent.concurrency, 10);
    assert!(ent.has_feature("batch"));
}

#[tokio::test]
async fn effective_plan_defaults_to_free() {
    let app = spawn_app().await;
    let user = app.seed_user_unique().await;
    let resolved = effective_plan(&app.state.plans, &app.pool, user.id)
        .await
        .expect("resolution reads");
    assert_eq!(resolved.plan.code, "free");
    assert_eq!(resolved.source, PlanSource::FreeDefault);
    assert_eq!(resolved.entitlements.daily_images, 10);
    assert_eq!(resolved.entitlements.daily_video_seconds, 0);
    assert!(!resolved.entitlements.has_feature("api_keys"));
}

#[tokio::test]
async fn effective_plan_trialing_subscription_overrides() {
    let app = spawn_app().await;
    let user = app.seed_user_unique().await;
    seed_subscription(&app, user.id, "pro", "trialing", Utc::now()).await;
    let resolved = effective_plan(&app.state.plans, &app.pool, user.id)
        .await
        .expect("resolution reads");
    assert_eq!(resolved.plan.code, "pro");
    assert_eq!(resolved.source, PlanSource::Subscription);
    assert_eq!(resolved.entitlements.daily_images, 500);
    assert!(resolved.entitlements.has_feature("api_keys"));
}

#[tokio::test]
async fn effective_plan_active_subscription_overrides() {
    let app = spawn_app().await;
    let user = app.seed_user_unique().await;
    seed_subscription(&app, user.id, "business", "active", Utc::now()).await;
    let resolved = effective_plan(&app.state.plans, &app.pool, user.id)
        .await
        .expect("resolution reads");
    assert_eq!(resolved.plan.code, "business");
    assert_eq!(resolved.source, PlanSource::Subscription);
    assert_eq!(resolved.entitlements.concurrency, 10);
    assert_eq!(
        resolved.entitlements.lane,
        detag_core::plans::Lane::Priority
    );
    assert!(resolved.entitlements.has_feature("batch"));
}

#[tokio::test]
async fn effective_plan_past_due_and_canceled_do_not_count() {
    let app = spawn_app().await;
    // A past_due Pro subscription grants nothing.
    let past_due_user = app.seed_user_unique().await;
    seed_subscription(&app, past_due_user.id, "pro", "past_due", Utc::now()).await;
    let resolved = effective_plan(&app.state.plans, &app.pool, past_due_user.id)
        .await
        .expect("resolution reads");
    assert_eq!(resolved.plan.code, "free", "past_due grants nothing");
    assert_eq!(resolved.source, PlanSource::FreeDefault);

    // A canceled Business subscription grants nothing either.
    let canceled_user = app.seed_user_unique().await;
    seed_subscription(&app, canceled_user.id, "business", "canceled", Utc::now()).await;
    let resolved = effective_plan(&app.state.plans, &app.pool, canceled_user.id)
        .await
        .expect("resolution reads");
    assert_eq!(resolved.plan.code, "free", "canceled grants nothing");
    assert_eq!(resolved.source, PlanSource::FreeDefault);
}

#[tokio::test]
async fn effective_plan_newest_live_subscription_wins() {
    let app = spawn_app().await;
    let user = app.seed_user_unique().await;
    let now = Utc::now();
    // An older active Pro subscription…
    seed_subscription(&app, user.id, "pro", "active", now - TimeDelta::hours(2)).await;
    // …overridden by a newer active Business subscription.
    seed_subscription(&app, user.id, "business", "active", now).await;
    let resolved = effective_plan(&app.state.plans, &app.pool, user.id)
        .await
        .expect("resolution reads");
    assert_eq!(resolved.plan.code, "business", "newest live wins");
    assert_eq!(resolved.source, PlanSource::Subscription);

    // And the reverse order: a newer Pro beats an older
    // Business, so the winner is the newest row, not the
    // most expensive plan.
    let user2 = app.seed_user_unique().await;
    seed_subscription(
        &app,
        user2.id,
        "business",
        "active",
        now - TimeDelta::hours(2),
    )
    .await;
    seed_subscription(&app, user2.id, "pro", "active", now).await;
    let resolved = effective_plan(&app.state.plans, &app.pool, user2.id)
        .await
        .expect("resolution reads");
    assert_eq!(resolved.plan.code, "pro", "newest live wins, not dearest");
}

/// A store that returns no plans at all — the stand-in for
/// a database whose seed never ran.
struct EmptyStore;

impl PlanStore for EmptyStore {
    async fn all(&self) -> Result<Vec<plans::Plan>, RepoError> {
        Ok(vec![])
    }
}

#[tokio::test]
async fn effective_plan_missing_free_plan_is_an_error() {
    let app = spawn_app().await;
    let user = app.seed_user_unique().await;
    let catalog = PlanCatalog::new(EmptyStore, InMemoryPlanCache::new());
    let err = effective_plan(&catalog, &app.pool, user.id)
        .await
        .expect_err("no free plan must error");
    assert!(
        matches!(err, EntitlementError::MissingFreePlan),
        "a missing free plan is MissingFreePlan, not a silent default: {err:?}"
    );
}
