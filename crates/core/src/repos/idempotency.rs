//! Idempotency-key response store (P1-062, FR-P-03).
//!
//! `idempotency_keys` (migration 20261112065339, data-model
//! doc): one row per client-supplied key — the key is the
//! PRIMARY KEY, so a second insert for the same key is a
//! unique violation and the first committer wins. The row
//! stores the whole envelope: `response` is `{"request": <the
//! canonical request the first call made>, "response": <the
//! response it got>}`. A replay compares the stored `request`
//! with the new call's canonical request — equal means "same
//! request, return the stored response without calling the
//! provider again"; different means a payload mismatch
//! (409 `PF-2011` — the taxonomy's idempotency code).
//!
//! The `request` half is stored, not a hash of it: the
//! comparison is exact JSON equality, so no hash function (and
//! no canonicalization argument) is needed — the service builds
//! the canonical JSON once and both sides compare the same
//! value. The `endpoint` column scopes the key to one route
//! (a key reused on another endpoint hits the same row, and
//! the endpoint inside the stored `request` makes the
//! fingerprints differ, so it 409s rather than replaying a
//! foreign response).
//!
//! Rows expire: `expires_at` is the instant after which the
//! key is dead — a replay past it is a fresh request (the
//! caller deletes the stale row first, so the new response
//! can take the key). For checkout the TTL is 24 h, matching
//! Stripe's checkout-session lifetime (ADR-0030).
//!
//! The caller (the checkout service) owns the sequence —
//! read, provider call, store — and the race between two
//! concurrent same-key requests: both read "no row", both call
//! the provider, and the loser's `insert` returns `false`
//! (`ON CONFLICT DO NOTHING`); the loser then re-reads and
//! replays the winner's stored response when the fingerprints
//! agree. A concurrent retry can therefore create two Stripe
//! sessions; a sequential retry never does (the AC's case).
//! The table has no cleanup job yet: expired rows are deleted
//! lazily on reuse of their key (P1-082's nightly sweep can
//! take the rest).

use super::error::RepoError;
use chrono::{DateTime, Utc};
use serde_json::Value;
use uuid::Uuid;

/// A stored idempotency row.
#[derive(Debug, Clone, PartialEq)]
pub struct IdempotencyRow {
    /// The client-supplied key (the table's primary key).
    pub key: String,
    /// The user the first call belonged to (`None` only for
    /// anonymous endpoints — checkout always sets it).
    pub user_id: Option<Uuid>,
    /// The route the first call hit (`/v1/billing/checkout`).
    pub endpoint: String,
    /// The envelope: `{"request": <canonical request JSON>,
    /// "response": <the response JSON>}`.
    pub response: Value,
    /// After this instant the key is dead (replay is a fresh
    /// request).
    pub expires_at: DateTime<Utc>,
}

/// Read a live row: `None` when the key was never stored or
/// its `expires_at` has passed. The caller passes the app
/// clock's `now` — a frozen clock makes expiry testable
/// (ADR-0027's rule, applied here too).
pub async fn get(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    key: &str,
    now: DateTime<Utc>,
) -> Result<Option<IdempotencyRow>, RepoError> {
    sqlx::query_as!(
        IdempotencyRow,
        "SELECT key, user_id, endpoint, response, expires_at \
         FROM idempotency_keys WHERE key = $1 AND expires_at > $2",
        key,
        now,
    )
    .fetch_optional(pool)
    .await
    .map_err(RepoError::from_sqlx)
}

/// Store a response. Returns `true` when this call stored the
/// row, `false` when another request already holds the key
/// (the caller re-reads and replays the winner's response).
/// An expired row for the same key does NOT block the insert
/// — the caller deletes it first ([`delete_expired`]).
pub async fn insert(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    key: &str,
    user_id: Option<Uuid>,
    endpoint: &str,
    response: &Value,
    expires_at: DateTime<Utc>,
) -> Result<bool, RepoError> {
    let result = sqlx::query!(
        "INSERT INTO idempotency_keys (key, user_id, endpoint, response, expires_at) \
         VALUES ($1, $2, $3, $4, $5) ON CONFLICT (key) DO NOTHING",
        key,
        user_id,
        endpoint,
        response,
        expires_at,
    )
    .execute(pool)
    .await
    .map_err(RepoError::from_sqlx)?;
    Ok(result.rows_affected() == 1)
}

/// Delete an expired row for this key, if one exists (a live
/// row is untouched). The caller runs this before storing a
/// fresh response under a key whose old row expired — without
/// it the `ON CONFLICT DO NOTHING` insert would silently
/// lose to a dead row.
pub async fn delete_expired(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    key: &str,
    now: DateTime<Utc>,
) -> Result<(), RepoError> {
    sqlx::query!(
        "DELETE FROM idempotency_keys WHERE key = $1 AND expires_at <= $2",
        key,
        now,
    )
    .execute(pool)
    .await
    .map_err(RepoError::from_sqlx)?;
    Ok(())
}
