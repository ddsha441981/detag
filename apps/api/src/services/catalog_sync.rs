//! Service: run the products/prices catalog sync (P1-061, FR-P-05).
//!
//! The admin endpoint (`POST /v1/admin/billing/catalog-sync`)
//! calls [`run_catalog_sync`]: build the Stripe adapter from
//! settings, sync the plan catalog through
//! [`detag_billing::sync_catalog`], then persist the changed
//! `stripe_price_id` values back to `plans` in one transaction
//! and invalidate the plan cache.
//!
//! The provider runs before the DB writes on purpose: if the
//! write fails after Stripe prices were created, the next sync
//! adopts the already-matching prices (`PriceAdopted`) instead
//! of creating duplicates — a half-applied sync heals itself.
//! The sync never touches a live key: the adapter is built with
//! `allow_live = false` always (owner decision #3, still
//! pending — test mode only).

use crate::AppState;
use detag_billing::{sync_catalog, CatalogDriftReport, CatalogPlan, StripeAdapter};
use detag_core::{
    db::PoolConfig,
    repos::{plans, RepoError},
    tx::{self, TxError},
};

/// What a catalog sync can fail with.
#[derive(Debug, thiserror::Error)]
pub enum CatalogSyncError {
    /// No Stripe secret key is configured (or the configured
    /// key is malformed / a live key without the owner flag):
    /// billing is not configured. The handler answers 503
    /// `PF-5005`.
    #[error("billing provider not configured")]
    NotConfigured,
    /// The provider call failed (the product list — the only
    /// sync step that aborts the whole run). The handler
    /// answers 500 `PF-5000`; the provider message never
    /// reaches the problem body.
    #[error("billing provider error")]
    Provider,
    /// The catalog read or the price-id write failed.
    #[error("catalog sync failed")]
    Internal,
}

/// The transaction closure's error: small on purpose — it must
/// not recurse into [`CatalogSyncError`].
#[derive(Debug)]
enum SyncTxError {
    NotFound,
    Db(RepoError),
}

/// Run one catalog sync against the configured provider and
/// persist the changed price ids.
///
/// The adapter is built fresh per call from settings (cheap:
/// key validation + a client) — the sync needs a
/// `StripeAdapter` specifically (its `CatalogProvider` impl),
/// not the shared `Arc<dyn BillingProvider>` `AppState` holds
/// for checkout (P1-062), so the two constructions stay
/// separate on purpose.
pub async fn run_catalog_sync(state: &AppState) -> Result<CatalogDriftReport, CatalogSyncError> {
    // Empty or the dev placeholder means billing is not
    // configured — never build an adapter from a sentinel
    // (it would pass the `sk_test_` prefix check and then
    // attempt a live call with a key that is not a key).
    // The check lives on `Settings` now (P1-062): the
    // checkout service and `AppState` share it.
    if !state.settings.stripe_configured() {
        return Err(CatalogSyncError::NotConfigured);
    }
    let adapter = StripeAdapter::new(
        &state.settings.stripe_secret_key,
        &state.settings.stripe_webhook_secret,
        false,
    )
    .map_err(|_| CatalogSyncError::NotConfigured)?;
    let catalog = state
        .plans
        .catalog()
        .await
        .map_err(|_| CatalogSyncError::Internal)?;
    let plans: Vec<CatalogPlan> = catalog
        .iter()
        .map(|plan| CatalogPlan {
            code: plan.code.clone(),
            amount: plan.amount,
            currency: plan.currency.clone(),
            stripe_price_id: plan.stripe_price_id.clone(),
        })
        .collect();
    let report = sync_catalog(&adapter, &plans)
        .await
        .map_err(|_| CatalogSyncError::Provider)?;
    let updates: Vec<(String, String)> = report
        .price_id_updates()
        .iter()
        .map(|(code, price_id)| (code.to_string(), price_id.to_string()))
        .collect();
    if !updates.is_empty() {
        let timeout = PoolConfig::default().transaction_timeout;
        tx::with_tx(&state.pool, timeout, |conn| {
            let updates = updates.clone();
            Box::pin(async move {
                for (code, price_id) in &updates {
                    let current = plans::by_code(&mut *conn, code)
                        .await
                        .map_err(SyncTxError::Db)?
                        .ok_or(SyncTxError::NotFound)?;
                    plans::update_by_code(
                        &mut *conn,
                        code,
                        current.amount,
                        &current.currency,
                        Some(price_id),
                        &current.entitlements,
                    )
                    .await
                    .map_err(SyncTxError::Db)?
                    .ok_or(SyncTxError::NotFound)?;
                }
                Ok(())
            })
        })
        .await
        .map_err(|err| match err {
            TxError::Inner(SyncTxError::NotFound) => {
                tracing::error!("catalog sync: plan vanished mid-sync");
                CatalogSyncError::Internal
            }
            TxError::Inner(SyncTxError::Db(db_err)) => {
                tracing::error!(error = ?db_err, "catalog sync: price-id write failed");
                CatalogSyncError::Internal
            }
            other => {
                tracing::error!(error = ?other, "catalog sync: transaction failed");
                CatalogSyncError::Internal
            }
        })?;
        // The price ids committed — drop the cached catalog so
        // the next read (checkout in P1-062, `/v1/me`) sees
        // them at once.
        state.plans.invalidate();
    }
    Ok(report)
}
