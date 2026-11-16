//! detag-billing::sync — the products/prices catalog sync (P1-061, FR-P-05).
//!
//! The plan catalog lives in our DB (`plans`, P1-049); Stripe needs
//! a product + a recurring monthly price per paid plan before
//! checkout can sell it (P1-062 reads `plans.stripe_price_id`).
//! This module is the sync between the two: [`sync_catalog`]
//! compares every plan against what the provider has and repairs
//! the drift, returning a [`CatalogDriftReport`] that names every
//! action taken. The admin endpoint (`POST
//! /v1/admin/billing/catalog-sync`) runs it on demand; the nightly
//! schedule that runs it unattended is P1-082's (same shape as the
//! P1-050 reconciliation — no scheduler exists yet, ASSUMPTION-024).
//!
//! # Why not a `BillingProvider` method (ASSUMPTION-024)
//!
//! The trait (P1-059, ADR-0028) is deliberately a narrow,
//! provider-agnostic contract over domain types — checkout /
//! portal / subscription / invoices / customers / webhooks. Products
//! and prices are Stripe-shaped concepts (Razorpay calls them
//! "plans" and prices them differently); putting them on the trait
//! would force a Razorpay implementation that does not exist yet
//! and leak Stripe concepts across the contract. The sync is
//! therefore adapter-local: a small internal [`CatalogProvider`]
//! trait (list / create product, list / create / archive price)
//! implemented for [`StripeAdapter`](crate::stripe::StripeAdapter)
//! in `stripe.rs`, and the engine here is generic over it. The
//! engine never names Stripe: the report says `provider: "stripe"`
//! because the caller passed the Stripe adapter, not because the
//! engine knows.
//!
//! # Rules
//!
//! * **Free plans are skipped** — a ₹0 plan has no Stripe price
//!   (there is nothing to sell); the report records `SkippedFree`
//!   and the `stripe_price_id` is left exactly as it is.
//! * **Products are matched by metadata**, not by name: the
//!   product carries `detag_plan_code = <code>`. A plan with no
//!   product gets one created (`Detag <Code>`).
//! * **Prices are immutable at Stripe** — a changed amount (or
//!   currency) never edits the old price: a new recurring monthly
//!   price is created and the old active one is archived. The
//!   report carries the `previous_price_id` so the change is
//!   auditable.
//! * **Adoption before creation** — a plan with no configured
//!   price that already has an active price for the same
//!   amount + currency adopts it (no duplicate price); only
//!   when no matching active price exists is one created.
//! * **Per-plan failure, not abort** — one plan's provider error
//!   is recorded as `Failed` and the sync continues with the
//!   rest; only a failure to *list* the products aborts the
//!   whole run (without the product list there is nothing to
//!   compare against).
//!
//! # What is (and is not) tested here
//!
//! The engine is proven against a `FakeCatalog` (the P1-059
//! `FakeProvider` pattern): in-memory products/prices with
//! scripted answers, covering in-sync, missing product, missing
//! price, amount drift (replace + archive), free skipped, and
//! per-plan failure. The Stripe implementation's HTTP legs are
//! NOT tested live — no Stripe test-mode key exists on this VM
//! and no network path to `api.stripe.com` was exercised
//! (ASSUMPTION-023 precedent); their first live run is the
//! admin endpoint with a test-mode key. The endpoint itself is
//! covered by its auth + unconfigured-provider tests
//! (`apps/api/tests/catalog_sync.rs`).

use std::collections::HashMap;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Serialize;
use utoipa::ToSchema;

use crate::error::BillingError;

/// The metadata key that ties a Stripe product to one of our plans.
/// Products are matched on this, never on the display name (names
/// are editable in the dashboard; the metadata is the contract).
pub const PLAN_CODE_METADATA_KEY: &str = "detag_plan_code";

/// One plan, as the sync sees it — the fields that decide what
/// Stripe must hold. The caller maps this from the `plans` row
/// (`detag_core::repos::plans::Plan`); this crate never depends
/// on core, so the mapping lives in the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogPlan {
    /// `free` / `pro` / `business`.
    pub code: String,
    /// Amount in the smallest currency unit (paise for INR).
    pub amount: i32,
    /// Three uppercase letters (`INR`).
    pub currency: String,
    /// The price Stripe should be selling, when configured.
    pub stripe_price_id: Option<String>,
}

/// One provider product, as the sync sees it — the id plus the
/// plan code from its metadata (when it carries one).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderProduct {
    /// The provider's product id (`prod_...`).
    pub id: String,
    /// Display name, when the provider reports one.
    pub name: Option<String>,
    /// The `detag_plan_code` metadata value, when present.
    pub plan_code: Option<String>,
}

/// One provider price, as the sync sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderPrice {
    /// The provider's price id (`price_...`).
    pub id: String,
    /// The product this price belongs to.
    pub product_id: String,
    /// Amount in the smallest currency unit.
    pub amount: i64,
    /// Three uppercase letters (`INR`).
    pub currency: String,
    /// Whether the price is usable for new purchases.
    pub active: bool,
}

/// The catalog operations the sync needs — the Stripe-shaped half
/// of the provider. Implemented for
/// [`StripeAdapter`](crate::stripe::StripeAdapter) in `stripe.rs`
/// (real `async-stripe` calls) and for the `FakeCatalog` in the
/// tests below. Deliberately separate from
/// [`BillingProvider`](crate::provider::BillingProvider): see the
/// module docs.
#[async_trait]
pub trait CatalogProvider: Send + Sync {
    /// The provider name for the report (`stripe`).
    fn provider_name(&self) -> &'static str;

    /// All products (up to the provider's page — 100, far above
    /// our three-plan catalog; see ASSUMPTION-024).
    async fn list_products(&self) -> Result<Vec<ProviderProduct>, BillingError>;

    /// Create the product for `plan` (name `Detag <Code>`,
    /// metadata `detag_plan_code = <code>`).
    async fn create_product(&self, plan: &CatalogPlan) -> Result<ProviderProduct, BillingError>;

    /// All prices for one product (active and archived — the
    /// sync needs the archived ones to tell "replaced" from
    /// "never existed").
    async fn list_prices(&self, product_id: &str) -> Result<Vec<ProviderPrice>, BillingError>;

    /// Create a recurring monthly price for `plan` on `product_id`.
    async fn create_price(
        &self,
        product_id: &str,
        plan: &CatalogPlan,
    ) -> Result<ProviderPrice, BillingError>;

    /// Archive a price (set inactive — prices are immutable, so
    /// archiving is how a price stops being sold).
    async fn archive_price(&self, price_id: &str) -> Result<(), BillingError>;
}

/// What the sync did for one plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SyncAction {
    /// The configured price exists, is active, and matches the
    /// plan's amount + currency — nothing to do.
    InSync,
    /// No product existed for the plan; one was created (and
    /// the price with it — a product with no price sells
    /// nothing).
    ProductCreated,
    /// No price existed for the plan; one was created.
    PriceCreated,
    /// The plan had no configured price but an active price
    /// for the same amount + currency already existed — adopted
    /// instead of creating a duplicate.
    PriceAdopted,
    /// The configured price no longer matches (amount/currency
    /// changed, or the price is gone/archived): a new price was
    /// created and the old active one archived.
    PriceReplaced,
    /// A ₹0 plan — no Stripe price exists for it, by design.
    SkippedFree,
    /// The provider failed for this plan; the sync continued
    /// with the rest. The `detail` carries the failure.
    Failed,
}

/// One plan's line in the drift report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct PlanSyncResult {
    /// The plan code (`free` / `pro` / `business`).
    pub plan_code: String,
    /// What the sync did.
    pub action: SyncAction,
    /// The product id after the sync, when the plan has one.
    pub product_id: Option<String>,
    /// The price id after the sync, when the plan has one.
    pub price_id: Option<String>,
    /// The price id this run replaced, when it replaced one.
    pub previous_price_id: Option<String>,
    /// True when this run archived the previous price. A
    /// replacement whose old price was already inactive (or
    /// missing) archives nothing — the count of archived prices
    /// reads this flag, not the action.
    pub old_price_archived: bool,
    /// Human detail — what was created / adopted / archived,
    /// or the provider failure for `Failed`.
    pub detail: String,
}

/// The drift report: one line per plan plus the counts. This is
/// the ticket's AC — the report is produced by every sync run,
/// returned by the admin endpoint, and logged by the nightly job.
/// Serializable so the endpoint can return it as JSON unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct CatalogDriftReport {
    /// When the sync ran.
    pub generated_at: DateTime<Utc>,
    /// The provider that was synced (`stripe`).
    pub provider: String,
    /// One line per plan, in catalog order (cheapest first, as
    /// the caller passes them).
    pub plans: Vec<PlanSyncResult>,
    /// Products created by this run.
    pub products_created: usize,
    /// Prices created by this run.
    pub prices_created: usize,
    /// Prices archived by this run.
    pub prices_archived: usize,
    /// Plans already in sync.
    pub in_sync: usize,
    /// Plans skipped (₹0 plans).
    pub skipped: usize,
    /// Plans that failed.
    pub failed: usize,
}

impl CatalogDriftReport {
    /// The `plans.stripe_price_id` writes the caller must persist:
    /// `(plan_code, price_id)` for every plan whose price changed
    /// this run (created, adopted, or replaced). In-sync, skipped
    /// and failed plans write nothing — the DB already holds (or
    /// must keep holding) what it holds.
    #[must_use]
    pub fn price_id_updates(&self) -> Vec<(&str, &str)> {
        self.plans
            .iter()
            .filter(|p| {
                matches!(
                    p.action,
                    SyncAction::ProductCreated
                        | SyncAction::PriceCreated
                        | SyncAction::PriceAdopted
                        | SyncAction::PriceReplaced
                )
            })
            .filter_map(|p| {
                p.price_id
                    .as_deref()
                    .map(|price_id| (p.plan_code.as_str(), price_id))
            })
            .collect()
    }
}

/// The product display name for a plan code (`Detag Pro`).
/// Shared with the Stripe adapter so the engine and the adapter
/// never disagree on the name.
#[must_use]
pub fn product_name(code: &str) -> String {
    let mut chars = code.chars();
    match chars.next() {
        Some(first) => format!("Detag {}{}", first.to_ascii_uppercase(), chars.as_str()),
        None => "Detag".to_owned(),
    }
}

/// Compare amounts + currencies the way the sync does: the plan
/// amount (i32 paise) against the price amount (i64), currencies
/// case-insensitively (`INR` vs Stripe's `inr`).
fn price_matches(plan: &CatalogPlan, price: &ProviderPrice) -> bool {
    price.active
        && i64::from(plan.amount) == price.amount
        && plan.currency.eq_ignore_ascii_case(&price.currency)
}

/// Sync the plan catalog to the provider.
///
/// Loads the provider's products once, then walks the plans in
/// the order given. Returns the drift report; a failure to list
/// the products aborts with [`BillingError`] (there is nothing
/// to compare against), while a per-plan failure is recorded on
/// that plan's line and the sync continues.
pub async fn sync_catalog(
    provider: &impl CatalogProvider,
    plans: &[CatalogPlan],
) -> Result<CatalogDriftReport, BillingError> {
    let products = provider.list_products().await?;
    let mut by_code: HashMap<&str, &ProviderProduct> = HashMap::new();
    for product in &products {
        if let Some(code) = &product.plan_code {
            by_code.insert(code.as_str(), product);
        }
    }

    let mut report = CatalogDriftReport {
        generated_at: Utc::now(),
        provider: provider.provider_name().to_owned(),
        plans: Vec::with_capacity(plans.len()),
        products_created: 0,
        prices_created: 0,
        prices_archived: 0,
        in_sync: 0,
        skipped: 0,
        failed: 0,
    };

    for plan in plans {
        let line = sync_one_plan(provider, plan, by_code.get(plan.code.as_str()).copied()).await;
        match line.action {
            SyncAction::InSync => report.in_sync += 1,
            SyncAction::ProductCreated => {
                report.products_created += 1;
                report.prices_created += 1;
            }
            // Adoption creates nothing — the price already existed.
            SyncAction::PriceAdopted => {}
            SyncAction::PriceCreated => report.prices_created += 1,
            SyncAction::PriceReplaced => {
                report.prices_created += 1;
                if line.old_price_archived {
                    report.prices_archived += 1;
                }
            }
            SyncAction::SkippedFree => report.skipped += 1,
            SyncAction::Failed => report.failed += 1,
        }
        report.plans.push(line);
    }
    Ok(report)
}

/// Sync one plan. `existing` is the provider product already
/// carrying this plan's metadata code, when one exists.
async fn sync_one_plan(
    provider: &impl CatalogProvider,
    plan: &CatalogPlan,
    existing: Option<&ProviderProduct>,
) -> PlanSyncResult {
    let fail = |detail: String| PlanSyncResult {
        plan_code: plan.code.clone(),
        action: SyncAction::Failed,
        product_id: existing.map(|p| p.id.clone()),
        price_id: plan.stripe_price_id.clone(),
        previous_price_id: None,
        old_price_archived: false,
        detail,
    };

    // A ₹0 plan has no Stripe price — nothing to sync, and the
    // configured price id (if any) is left exactly as it is.
    if plan.amount == 0 {
        return PlanSyncResult {
            plan_code: plan.code.clone(),
            action: SyncAction::SkippedFree,
            product_id: existing.map(|p| p.id.clone()),
            price_id: plan.stripe_price_id.clone(),
            previous_price_id: None,
            old_price_archived: false,
            detail: "₹0 plan has no Stripe price".to_owned(),
        };
    }

    // Resolve the product first: create it when the plan has none.
    let (product, product_created) = match existing {
        Some(product) => (product.clone(), false),
        None => match provider.create_product(plan).await {
            Ok(product) => (product, true),
            Err(err) => return fail(format!("create product failed: {err}")),
        },
    };
    let product_id = product.id.clone();

    let prices = match provider.list_prices(&product_id).await {
        Ok(prices) => prices,
        Err(err) => return fail(format!("list prices failed: {err}")),
    };

    // The configured price, when the provider still has it.
    let configured = plan
        .stripe_price_id
        .as_deref()
        .and_then(|id| prices.iter().find(|p| p.id == id));

    if let Some(price) = configured {
        if price_matches(plan, price) {
            return PlanSyncResult {
                plan_code: plan.code.clone(),
                action: if product_created {
                    // A product cannot be "created" while a matching
                    // price already exists for it — unreachable in
                    // practice (a new product has no prices), but
                    // report the price, not the product.
                    SyncAction::PriceCreated
                } else {
                    SyncAction::InSync
                },
                product_id: Some(product_id),
                price_id: Some(price.id.clone()),
                previous_price_id: None,
                old_price_archived: false,
                detail: format!(
                    "price {} matches {} {}",
                    price.id, plan.amount, plan.currency
                ),
            };
        }
        // The configured price exists but no longer matches (or is
        // archived): replace it — new price, archive the old one
        // when it is still active.
        return replace_price(provider, plan, &product_id, Some(price)).await;
    }

    // No configured price (or the configured one is gone from the
    // provider): adopt an active price that already matches, else
    // create one.
    if let Some(price) = prices.iter().find(|p| price_matches(plan, p)) {
        return PlanSyncResult {
            plan_code: plan.code.clone(),
            action: if product_created {
                SyncAction::ProductCreated
            } else {
                SyncAction::PriceAdopted
            },
            product_id: Some(product_id),
            price_id: Some(price.id.clone()),
            previous_price_id: plan.stripe_price_id.clone(),
            old_price_archived: false,
            detail: format!(
                "adopted existing price {} for {} {}",
                price.id, plan.amount, plan.currency
            ),
        };
    }
    match provider.create_price(&product_id, plan).await {
        Ok(price) => PlanSyncResult {
            plan_code: plan.code.clone(),
            action: if product_created {
                SyncAction::ProductCreated
            } else {
                SyncAction::PriceCreated
            },
            product_id: Some(product_id),
            price_id: Some(price.id.clone()),
            previous_price_id: plan.stripe_price_id.clone(),
            old_price_archived: false,
            detail: format!(
                "created price {} for {} {}",
                price.id, plan.amount, plan.currency
            ),
        },
        Err(err) => fail(format!("create price failed: {err}")),
    }
}

/// Replace the configured price: create the new one, then archive
/// the old one when it is still active. The create must succeed
/// first — archiving before the replacement exists would leave
/// the plan with no sellable price.
async fn replace_price(
    provider: &impl CatalogProvider,
    plan: &CatalogPlan,
    product_id: &str,
    old: Option<&ProviderPrice>,
) -> PlanSyncResult {
    let fail = |detail: String| PlanSyncResult {
        plan_code: plan.code.clone(),
        action: SyncAction::Failed,
        product_id: Some(product_id.to_owned()),
        price_id: plan.stripe_price_id.clone(),
        previous_price_id: old.map(|p| p.id.clone()),
        old_price_archived: false,
        detail,
    };
    let new_price = match provider.create_price(product_id, plan).await {
        Ok(price) => price,
        Err(err) => return fail(format!("create replacement price failed: {err}")),
    };
    if let Some(old) = old.filter(|p| p.active) {
        if let Err(err) = provider.archive_price(&old.id).await {
            // The new price exists and the DB will point at it;
            // the stale active price is drift the next sync
            // reports again — record it, do not fail the plan.
            return PlanSyncResult {
                plan_code: plan.code.clone(),
                action: SyncAction::PriceReplaced,
                product_id: Some(product_id.to_owned()),
                price_id: Some(new_price.id.clone()),
                previous_price_id: Some(old.id.clone()),
                old_price_archived: false,
                detail: format!(
                    "created price {} for {} {}; archiving {} failed: {err}",
                    new_price.id, plan.amount, plan.currency, old.id
                ),
            };
        }
    }
    PlanSyncResult {
        plan_code: plan.code.clone(),
        action: SyncAction::PriceReplaced,
        product_id: Some(product_id.to_owned()),
        price_id: Some(new_price.id.clone()),
        previous_price_id: old.map(|p| p.id.clone()),
        old_price_archived: old.map(|p| p.active).unwrap_or(false),
        detail: format!(
            "replaced price {} with {} for {} {}",
            old.map(|p| p.id.as_str()).unwrap_or("<missing>"),
            new_price.id,
            plan.amount,
            plan.currency
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A test double: canned products/prices, recording every
    /// write. The `fail_*` flags script one provider failure
    /// each (the P1-059 `FakeProvider` pattern).
    struct FakeCatalog {
        products: Vec<ProviderProduct>,
        prices: Vec<ProviderPrice>,
        created_products: Mutex<Vec<String>>,
        created_prices: Mutex<Vec<(String, i64, String)>>,
        archived_prices: Mutex<Vec<String>>,
        fail_list_products: bool,
        fail_create_product: bool,
        fail_list_prices_for: Mutex<Vec<String>>,
        fail_create_price: bool,
        next_product: Mutex<u32>,
        next_price: Mutex<u32>,
    }

    impl FakeCatalog {
        fn new(products: Vec<ProviderProduct>, prices: Vec<ProviderPrice>) -> Self {
            Self {
                products,
                prices,
                created_products: Mutex::new(Vec::new()),
                created_prices: Mutex::new(Vec::new()),
                archived_prices: Mutex::new(Vec::new()),
                fail_list_products: false,
                fail_create_product: false,
                fail_list_prices_for: Mutex::new(Vec::new()),
                fail_create_price: false,
                next_product: Mutex::new(100),
                next_price: Mutex::new(100),
            }
        }

        fn product(code: &str, id: &str) -> ProviderProduct {
            ProviderProduct {
                id: id.to_owned(),
                name: Some(format!("Detag {code}")),
                plan_code: Some(code.to_owned()),
            }
        }

        fn price(id: &str, product_id: &str, amount: i64, active: bool) -> ProviderPrice {
            ProviderPrice {
                id: id.to_owned(),
                product_id: product_id.to_owned(),
                amount,
                currency: "INR".to_owned(),
                active,
            }
        }
    }

    #[async_trait]
    impl CatalogProvider for FakeCatalog {
        fn provider_name(&self) -> &'static str {
            "stripe"
        }

        async fn list_products(&self) -> Result<Vec<ProviderProduct>, BillingError> {
            if self.fail_list_products {
                return Err(BillingError::Provider("list products boom".to_owned()));
            }
            Ok(self.products.clone())
        }

        async fn create_product(
            &self,
            plan: &CatalogPlan,
        ) -> Result<ProviderProduct, BillingError> {
            if self.fail_create_product {
                return Err(BillingError::Provider("create product boom".to_owned()));
            }
            let mut next = self.next_product.lock().expect("lock");
            let id = format!("prod_fake_{next}");
            *next += 1;
            self.created_products
                .lock()
                .expect("lock")
                .push(plan.code.clone());
            Ok(ProviderProduct {
                id,
                name: Some(product_name(&plan.code)),
                plan_code: Some(plan.code.clone()),
            })
        }

        async fn list_prices(&self, product_id: &str) -> Result<Vec<ProviderPrice>, BillingError> {
            if self
                .fail_list_prices_for
                .lock()
                .expect("lock")
                .contains(&product_id.to_owned())
            {
                return Err(BillingError::Provider("list prices boom".to_owned()));
            }
            Ok(self
                .prices
                .iter()
                .filter(|p| p.product_id == product_id)
                .cloned()
                .collect())
        }

        async fn create_price(
            &self,
            product_id: &str,
            plan: &CatalogPlan,
        ) -> Result<ProviderPrice, BillingError> {
            if self.fail_create_price {
                return Err(BillingError::Provider("create price boom".to_owned()));
            }
            let mut next = self.next_price.lock().expect("lock");
            let id = format!("price_fake_{next}");
            *next += 1;
            self.created_prices.lock().expect("lock").push((
                product_id.to_owned(),
                i64::from(plan.amount),
                plan.currency.clone(),
            ));
            Ok(ProviderPrice {
                id,
                product_id: product_id.to_owned(),
                amount: i64::from(plan.amount),
                currency: plan.currency.clone(),
                active: true,
            })
        }

        async fn archive_price(&self, price_id: &str) -> Result<(), BillingError> {
            self.archived_prices
                .lock()
                .expect("lock")
                .push(price_id.to_owned());
            Ok(())
        }
    }

    fn plan(code: &str, amount: i32, price_id: Option<&str>) -> CatalogPlan {
        CatalogPlan {
            code: code.to_owned(),
            amount,
            currency: "INR".to_owned(),
            stripe_price_id: price_id.map(str::to_owned),
        }
    }

    #[tokio::test]
    async fn in_sync_free_skipped_and_adopted_in_one_run() {
        let catalog = FakeCatalog::new(
            vec![
                FakeCatalog::product("pro", "prod_pro"),
                FakeCatalog::product("business", "prod_biz"),
            ],
            vec![
                FakeCatalog::price("price_pro", "prod_pro", 49_900, true),
                FakeCatalog::price("price_biz", "prod_biz", 199_900, true),
            ],
        );
        let plans = vec![
            plan("free", 0, None),
            plan("pro", 49_900, Some("price_pro")),
            // Business has no configured price but an active
            // matching one already exists — adopted, not created.
            plan("business", 199_900, None),
        ];
        let report = sync_catalog(&catalog, &plans).await.expect("sync runs");
        assert_eq!(report.provider, "stripe");
        assert_eq!(report.plans.len(), 3);
        assert_eq!(report.plans[0].action, SyncAction::SkippedFree);
        assert_eq!(report.plans[1].action, SyncAction::InSync);
        assert_eq!(report.plans[1].price_id.as_deref(), Some("price_pro"));
        assert_eq!(report.plans[2].action, SyncAction::PriceAdopted);
        assert_eq!(report.plans[2].price_id.as_deref(), Some("price_biz"));
        assert_eq!(report.in_sync, 1);
        assert_eq!(report.skipped, 1);
        assert_eq!(report.prices_created, 0);
        assert_eq!(report.products_created, 0);
        assert_eq!(report.failed, 0);
        // Only the adoption writes back to the DB.
        assert_eq!(report.price_id_updates(), vec![("business", "price_biz")]);
        assert!(catalog.created_products.lock().expect("lock").is_empty());
        assert!(catalog.created_prices.lock().expect("lock").is_empty());
    }

    #[tokio::test]
    async fn missing_product_and_price_are_created() {
        let catalog = FakeCatalog::new(vec![], vec![]);
        let plans = vec![plan("pro", 49_900, None)];
        let report = sync_catalog(&catalog, &plans).await.expect("sync runs");
        assert_eq!(report.plans[0].action, SyncAction::ProductCreated);
        assert_eq!(report.plans[0].product_id.as_deref(), Some("prod_fake_100"));
        assert_eq!(report.plans[0].price_id.as_deref(), Some("price_fake_100"));
        assert_eq!(report.products_created, 1);
        assert_eq!(report.prices_created, 1);
        assert_eq!(
            catalog.created_products.lock().expect("lock").as_slice(),
            ["pro"]
        );
        assert_eq!(report.price_id_updates(), vec![("pro", "price_fake_100")]);
    }

    #[tokio::test]
    async fn amount_drift_replaces_and_archives_the_old_price() {
        let catalog = FakeCatalog::new(
            vec![FakeCatalog::product("pro", "prod_pro")],
            vec![FakeCatalog::price("price_old", "prod_pro", 39_900, true)],
        );
        // The plan's price rose 39_900 -> 49_900; the configured
        // price no longer matches.
        let plans = vec![plan("pro", 49_900, Some("price_old"))];
        let report = sync_catalog(&catalog, &plans).await.expect("sync runs");
        assert_eq!(report.plans[0].action, SyncAction::PriceReplaced);
        assert_eq!(report.plans[0].price_id.as_deref(), Some("price_fake_100"));
        assert_eq!(
            report.plans[0].previous_price_id.as_deref(),
            Some("price_old")
        );
        assert_eq!(report.prices_created, 1);
        assert_eq!(report.prices_archived, 1);
        assert_eq!(
            catalog.archived_prices.lock().expect("lock").as_slice(),
            ["price_old"]
        );
        assert_eq!(report.price_id_updates(), vec![("pro", "price_fake_100")]);
    }

    #[tokio::test]
    async fn configured_price_gone_from_provider_is_recreated() {
        let catalog = FakeCatalog::new(vec![FakeCatalog::product("pro", "prod_pro")], vec![]);
        // The DB points at a price Stripe no longer has (deleted
        // in the dashboard): a fresh price is created, and the
        // stale id is recorded as the previous one.
        let plans = vec![plan("pro", 49_900, Some("price_deleted"))];
        let report = sync_catalog(&catalog, &plans).await.expect("sync runs");
        assert_eq!(report.plans[0].action, SyncAction::PriceCreated);
        assert_eq!(
            report.plans[0].previous_price_id.as_deref(),
            Some("price_deleted")
        );
        assert_eq!(report.prices_archived, 0);
        assert_eq!(report.price_id_updates(), vec![("pro", "price_fake_100")]);
    }

    #[tokio::test]
    async fn archived_configured_price_is_replaced_not_adopted() {
        let catalog = FakeCatalog::new(
            vec![FakeCatalog::product("pro", "prod_pro")],
            vec![FakeCatalog::price("price_old", "prod_pro", 49_900, false)],
        );
        // Same amount, but the configured price is archived — it
        // cannot be sold, so a new one is created and the old one
        // (already inactive) is not archived again.
        let plans = vec![plan("pro", 49_900, Some("price_old"))];
        let report = sync_catalog(&catalog, &plans).await.expect("sync runs");
        assert_eq!(report.plans[0].action, SyncAction::PriceReplaced);
        assert_eq!(report.prices_archived, 0);
        assert!(catalog.archived_prices.lock().expect("lock").is_empty());
    }

    #[tokio::test]
    async fn per_plan_failure_is_recorded_and_the_sync_continues() {
        let catalog = FakeCatalog::new(
            vec![
                FakeCatalog::product("pro", "prod_pro"),
                FakeCatalog::product("business", "prod_biz"),
            ],
            vec![FakeCatalog::price("price_pro", "prod_pro", 49_900, true)],
        );
        catalog
            .fail_list_prices_for
            .lock()
            .expect("lock")
            .push("prod_biz".to_owned());
        let plans = vec![
            plan("pro", 49_900, Some("price_pro")),
            plan("business", 199_900, None),
        ];
        let report = sync_catalog(&catalog, &plans).await.expect("sync runs");
        assert_eq!(report.plans[0].action, SyncAction::InSync);
        assert_eq!(report.plans[1].action, SyncAction::Failed);
        assert!(report.plans[1].detail.contains("list prices failed"));
        assert_eq!(report.failed, 1);
        assert_eq!(report.in_sync, 1);
        assert!(report.price_id_updates().is_empty());
    }

    #[tokio::test]
    async fn list_products_failure_aborts_the_whole_run() {
        let mut catalog = FakeCatalog::new(vec![], vec![]);
        catalog.fail_list_products = true;
        let err = sync_catalog(&catalog, &[plan("pro", 49_900, None)])
            .await
            .expect_err("no product list, no sync");
        assert_eq!(err, BillingError::Provider("list products boom".to_owned()));
    }

    #[tokio::test]
    async fn create_product_failure_fails_only_that_plan() {
        let mut catalog = FakeCatalog::new(vec![], vec![]);
        catalog.fail_create_product = true;
        let plans = vec![plan("pro", 49_900, None), plan("free", 0, None)];
        let report = sync_catalog(&catalog, &plans).await.expect("sync runs");
        assert_eq!(report.plans[0].action, SyncAction::Failed);
        assert!(report.plans[0].detail.contains("create product failed"));
        assert_eq!(report.plans[1].action, SyncAction::SkippedFree);
        assert_eq!(report.failed, 1);
    }

    #[test]
    fn product_name_capitalizes_the_code() {
        assert_eq!(product_name("pro"), "Detag Pro");
        assert_eq!(product_name("business"), "Detag Business");
        assert_eq!(product_name("free"), "Detag Free");
        assert_eq!(product_name(""), "Detag");
    }

    #[test]
    fn price_matching_is_case_insensitive_on_currency() {
        let plan = plan("pro", 49_900, None);
        let mut price = FakeCatalog::price("price_pro", "prod_pro", 49_900, true);
        price.currency = "inr".to_owned();
        assert!(price_matches(&plan, &price));
        price.currency = "USD".to_owned();
        assert!(!price_matches(&plan, &price));
        price.currency = "INR".to_owned();
        price.amount = 49_901;
        assert!(!price_matches(&plan, &price));
        price.amount = 49_900;
        price.active = false;
        assert!(!price_matches(&plan, &price));
    }

    #[test]
    fn report_serializes_for_the_admin_endpoint() {
        let report = CatalogDriftReport {
            generated_at: DateTime::from_timestamp(1_794_844_800, 0).expect("in range"),
            provider: "stripe".to_owned(),
            plans: vec![PlanSyncResult {
                plan_code: "pro".to_owned(),
                action: SyncAction::InSync,
                product_id: Some("prod_pro".to_owned()),
                price_id: Some("price_pro".to_owned()),
                previous_price_id: None,
                old_price_archived: false,
                detail: "price price_pro matches 49900 INR".to_owned(),
            }],
            products_created: 0,
            prices_created: 0,
            prices_archived: 0,
            in_sync: 1,
            skipped: 0,
            failed: 0,
        };
        let value = serde_json::to_value(&report).expect("report serializes");
        assert_eq!(value["provider"], "stripe");
        assert_eq!(value["plans"][0]["action"], "in_sync");
        assert_eq!(value["plans"][0]["price_id"], "price_pro");
        assert_eq!(value["in_sync"], 1);
    }
}
