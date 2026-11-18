//! Invoices list (P1-068, FR-P-03): `GET /v1/billing/invoices`.
//!
//! The caller's own invoices from the provider, newest first,
//! mapped to our [`Invoice`] shape — the Stripe-hosted page
//! (`hosted_url`) and the Stripe-hosted PDF download
//! (`download_url`) ride along, so the console never proxies
//! invoice files itself.
//!
//! Rules, in order:
//! - The customer comes from our own rows: the newest
//!   `provider_customer_id` on any of the user's
//!   subscription rows (live or canceled — the P1-062 reuse
//!   rule, the same lookup the portal uses). A user with no
//!   subscription row has no provider customer, so there is
//!   nothing to list: 200 with an empty list, and no provider
//!   call. A list endpoint answers "what do I have?" — for a
//!   user who never bought anything the honest answer is
//!   "none", not a 404 (the portal's 404 is for a page there
//!   is nothing to open; a list always exists, empty or not).
//! - Billing must be configured (`AppState::billing` is
//!   `Some`) — otherwise 503 `PF-5005`. The customer check
//!   runs first on purpose (the P1-062 ordering: local-data
//!   answers before the provider gate), so a user with no
//!   customer gets the empty list even while billing is
//!   unconfigured.
//! - The list is cached 5 minutes in Redis under
//!   `pf:cache:invoices:{customer_id}` (SETEX 300): the
//!   provider is called at most once per customer per
//!   window, and a second request inside the window is
//!   served from the cache. The cache never fails the
//!   request: a Redis error (or a corrupt cached value) is
//!   a miss — the provider is called and the fresh list is
//!   returned, with the failed write logged and ignored.
//!   Invoices change only when the provider issues one, and
//!   a 5-minute-old list is the documented staleness.
//! - A provider failure is 500 `PF-5000`, and no provider
//!   message ever reaches the problem body.
//! - No audit row: a read creates nothing durable. No
//!   idempotency: a GET is safe to retry, and the cache
//!   already dedups the provider call inside the window.

use std::time::Duration;

use deadpool_redis::redis::cmd;
use detag_billing::Invoice;
use detag_core::{
    redis::keys::cache_key,
    repos::{subscriptions, users::User},
};
use thiserror::Error;

use crate::AppState;

/// How long one customer's invoice list stays cached (the
/// ticket's "cached 5m").
pub const INVOICES_CACHE_TTL: Duration = Duration::from_secs(300);

/// How many invoices one call asks the provider for. The
/// endpoint takes no pagination parameter yet (the spec names
/// none) — 20 is one screen of invoices, and the provider
/// returns them newest first.
pub const INVOICES_LIMIT: u32 = 20;

/// What listing invoices can fail with. The handler maps each
/// variant to its problem body; the provider's messages never
/// leave this module.
#[derive(Debug, Error)]
pub enum InvoicesError {
    /// No billing provider is configured (no Stripe key):
    /// the handler answers 503 `PF-5005`.
    #[error("billing provider not configured")]
    NotConfigured,
    /// The provider call failed: 500 `PF-5000`.
    #[error("billing provider error")]
    Provider,
    /// The customer lookup failed: 500 `PF-5000`.
    #[error("invoices failed")]
    Internal,
}

/// List `user`'s invoices, newest first, from the provider
/// (cached 5 minutes per customer).
pub async fn list_invoices(state: &AppState, user: &User) -> Result<Vec<Invoice>, InvoicesError> {
    // Customer reuse: the newest provider customer id on any
    // of the user's subscription rows (live or canceled).
    let Some(customer_id) = subscriptions::latest_customer_id_for_user(&state.pool, user.id)
        .await
        .map_err(|_| InvoicesError::Internal)?
    else {
        // No subscription row, no provider customer, no
        // invoices — the empty list, with no provider call.
        return Ok(Vec::new());
    };

    // Billing must be configured to call the provider.
    let provider = state.billing.as_ref().ok_or(InvoicesError::NotConfigured)?;

    // The 5-minute cache: one provider call per customer per
    // window, a second request inside it served from Redis.
    let key = cache_key("invoices", &customer_id);
    if let Some(cached) = read_cached(state, &key).await {
        return Ok(cached);
    }
    let invoices = provider
        .list_invoices(&customer_id, INVOICES_LIMIT)
        .await
        .map_err(|err| {
            tracing::error!(error = %err, "invoices: provider call failed");
            InvoicesError::Provider
        })?;
    write_cached(state, &key, &invoices).await;
    Ok(invoices)
}

/// Read the cached list for `key`, if present and valid JSON.
/// A Redis error or a corrupt value is a miss (the caller
/// loads from the provider) — never an error.
async fn read_cached(state: &AppState, key: &str) -> Option<Vec<Invoice>> {
    let mut conn = match state.redis.get().await {
        Ok(conn) => conn,
        Err(err) => {
            tracing::warn!(error = %err, "invoices: redis pool checkout failed, cache miss");
            return None;
        }
    };
    let raw: Option<String> = match cmd("GET").arg(key).query_async(&mut conn).await {
        Ok(raw) => raw,
        Err(err) => {
            tracing::warn!(error = %err, "invoices: cache GET failed, cache miss");
            return None;
        }
    };
    match raw {
        None => None,
        Some(json) => match serde_json::from_str(&json) {
            Ok(invoices) => Some(invoices),
            Err(err) => {
                tracing::warn!(error = %err, "invoices: corrupt cached value, cache miss");
                None
            }
        },
    }
}

/// Store `invoices` under `key` for the cache TTL. Best
/// effort: a failure is logged and ignored — the caller
/// already has the fresh list to return.
async fn write_cached(state: &AppState, key: &str, invoices: &[Invoice]) {
    let json = match serde_json::to_string(invoices) {
        Ok(json) => json,
        Err(err) => {
            tracing::warn!(error = %err, "invoices: cached value is not serializable");
            return;
        }
    };
    let mut conn = match state.redis.get().await {
        Ok(conn) => conn,
        Err(err) => {
            tracing::warn!(error = %err, "invoices: redis pool checkout failed, not cached");
            return;
        }
    };
    if let Err(err) = cmd("SETEX")
        .arg(key)
        .arg(INVOICES_CACHE_TTL.as_secs() as i64)
        .arg(json)
        .query_async::<()>(&mut conn)
        .await
    {
        tracing::warn!(error = %err, "invoices: cache SETEX failed, not cached");
    }
}
