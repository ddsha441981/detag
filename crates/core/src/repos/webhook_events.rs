//! Provider webhook event ledger (P1-063, FR-P-04).
//!
//! `webhook_events` (migration 20261117021707, data-model doc):
//! one row per provider event, keyed by (`provider`,
//! `event_id`) — the UNIQUE constraint IS the idempotency.
//! The receiver inserts first (ON CONFLICT DO NOTHING); a
//! redelivery inserts nothing and is answered 200 without
//! dispatching twice. `payload` is the full envelope JSON
//! (the durable queue P1-065's handlers read from);
//! `dispatched_at` is stamped once the receiver has mapped
//! the event and handed it to the dispatcher — a replay
//! never reaches dispatch, so its stamp stays the first
//! delivery's. Rows are never deleted here (billing audit
//! trail, 365 d retention); a sweep is P1-082's if wanted.

use super::error::RepoError;
use chrono::{DateTime, Utc};
use serde_json::Value;

/// A stored provider event.
#[derive(Debug, Clone, PartialEq)]
pub struct WebhookEventRow {
    /// Our row id (not the provider's event id).
    pub id: uuid::Uuid,
    /// `stripe` (Razorpay later — the provider CHECK admits both).
    pub provider: String,
    /// The provider's event id (`evt_...`) — the idempotency key.
    pub event_id: String,
    /// The provider's event type (`checkout.session.completed`, ...).
    pub event_type: String,
    /// The full envelope JSON.
    pub payload: Value,
    /// When the first delivery arrived.
    pub received_at: DateTime<Utc>,
    /// When the receiver dispatched it (`None` until then).
    pub dispatched_at: Option<DateTime<Utc>>,
}

/// Store an event. Returns `true` when this call stored the
/// row, `false` when the event is already recorded (a replay —
/// the caller answers 200 and dispatches nothing).
pub async fn insert(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    provider: &str,
    event_id: &str,
    event_type: &str,
    payload: &Value,
) -> Result<bool, RepoError> {
    let result = sqlx::query!(
        "INSERT INTO webhook_events (provider, event_id, event_type, payload) \
         VALUES ($1, $2, $3, $4) ON CONFLICT (provider, event_id) DO NOTHING",
        provider,
        event_id,
        event_type,
        payload,
    )
    .execute(pool)
    .await
    .map_err(RepoError::from_sqlx)?;
    Ok(result.rows_affected() == 1)
}

/// Stamp `dispatched_at` once the event has been mapped and
/// handed to the dispatcher. Only the first stamp wins (the
/// `dispatched_at IS NULL` guard): a replay never reaches
/// dispatch, so it must not move the stamp either.
pub async fn mark_dispatched(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    provider: &str,
    event_id: &str,
    now: DateTime<Utc>,
) -> Result<(), RepoError> {
    sqlx::query!(
        "UPDATE webhook_events SET dispatched_at = $3 \
         WHERE provider = $1 AND event_id = $2 AND dispatched_at IS NULL",
        provider,
        event_id,
        now,
    )
    .execute(pool)
    .await
    .map_err(RepoError::from_sqlx)?;
    Ok(())
}

/// Read one stored event (`None` when never received).
pub async fn get(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    provider: &str,
    event_id: &str,
) -> Result<Option<WebhookEventRow>, RepoError> {
    sqlx::query_as!(
        WebhookEventRow,
        "SELECT id, provider, event_id, event_type, payload, \
                received_at, dispatched_at \
         FROM webhook_events WHERE provider = $1 AND event_id = $2",
        provider,
        event_id,
    )
    .fetch_optional(pool)
    .await
    .map_err(RepoError::from_sqlx)
}
