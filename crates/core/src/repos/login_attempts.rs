//! Failed-login counters repository (P1-004, E1.1).
//! `login_attempt_counters` is the durable lockout of record
//! (migration 20261113030740, data-model doc): one row per
//! (scope, key) — scope `account` keyed by the sha256 of the
//! email, scope `ip` keyed by the sha256 of the client IP.
//!
//! Race safety: [`record_failure`] runs inside the caller's
//! transaction and takes the row lock (`SELECT ... FOR
//! UPDATE`) before it reads the count — concurrent failures
//! serialize on the row, so no increment is ever lost, and
//! the lock is computed from the serialized count with
//! GREATEST against the existing `locked_until`, so it can
//! only ever extend, never shorten. The backoff math itself
//! lives in [`detag_core::lockout`] (the single source of
//! truth); this repo stores the result, it never re-implements
//! the curve.
//!
//! The caller (the login service) owns the transaction that
//! wraps a failure: counter record + failure audit + (on the
//! first lock) the lockout notification enqueue. The repo
//! functions take `&mut PgConnection` so they compose into
//! that transaction.

use super::error::RepoError;
use crate::lockout::{lock_seconds_for, LOCKOUT_WINDOW_SECS};
use chrono::{DateTime, Duration, Utc};
use sqlx::PgConnection;

/// The two counter scopes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockScope {
    /// Per account, keyed by the sha256 of the email.
    Account,
    /// Per client IP, keyed by the sha256 of the IP.
    Ip,
}

impl LockScope {
    pub fn as_str(self) -> &'static str {
        match self {
            LockScope::Account => "account",
            LockScope::Ip => "ip",
        }
    }
}

/// A stored counter row.
#[derive(Debug, Clone, PartialEq)]
pub struct Counter {
    pub scope: String,
    pub key: String,
    pub attempts: i32,
    pub first_failed_at: DateTime<Utc>,
    pub last_failed_at: DateTime<Utc>,
    pub locked_until: Option<DateTime<Utc>>,
    pub notified_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Counter {
    /// Whether the row is locked at `now`.
    pub fn is_locked_at(&self, now: DateTime<Utc>) -> bool {
        self.locked_until.is_some_and(|until| until > now)
    }
}

/// Read a counter row, if one exists. Plain read (no lock) —
/// the login service uses it for the lock check.
pub async fn get(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    scope: LockScope,
    key: &str,
) -> Result<Option<Counter>, RepoError> {
    sqlx::query_as!(
        Counter,
        r#"SELECT scope, key, attempts, first_failed_at, last_failed_at,
                  locked_until, notified_at, created_at, updated_at
           FROM login_attempt_counters WHERE scope = $1 AND key = $2"#,
        scope.as_str(),
        key,
    )
    .fetch_optional(pool)
    .await
    .map_err(RepoError::from_sqlx)
}

/// Record a failed login, atomically. `conn` is the caller's
/// transaction connection: the row is locked (`FOR UPDATE`)
/// before it is read, so concurrent failures serialize and no
/// increment is lost.
///
/// The row is ensured first with `INSERT ... ON CONFLICT DO
/// NOTHING` (a placeholder with `attempts = 0`): two
/// concurrent first-failures would otherwise both read "no
/// row" and both INSERT — one of them dies on the primary-key
/// conflict, and a failed statement poisons the whole
/// transaction (Postgres aborts it), so the loser would 500.
/// With the conflict swallowed, both transactions proceed to
/// the `SELECT ... FOR UPDATE`, which serializes them on the
/// row lock — the second reads the first's committed count and
/// increments it. The placeholder is only ever visible inside
/// its own transaction (the INSERT and the UPDATE below are
/// one transaction), so no committed row ever has
/// `attempts = 0`.
///
/// Returns the row after the record, plus whether THIS call
/// set the lock (the row was unlocked before, locked after).
/// The caller notifies (mail + `login.locked` audit) only
/// when `newly_locked` is true AND the count first reached
/// the threshold — extensions of an existing lock do not
/// re-notify (the user already knows).
pub async fn record_failure(
    conn: &mut PgConnection,
    scope: LockScope,
    key: &str,
    now: DateTime<Utc>,
) -> Result<(Counter, bool), RepoError> {
    sqlx::query!(
        r#"INSERT INTO login_attempt_counters
               (scope, key, attempts, first_failed_at, last_failed_at)
           VALUES ($1, $2, 0, $3, $3)
           ON CONFLICT (scope, key) DO NOTHING"#,
        scope.as_str(),
        key,
        now,
    )
    .execute(&mut *conn)
    .await
    .map_err(RepoError::from_sqlx)?;
    let row = sqlx::query_as!(
        Counter,
        r#"SELECT scope, key, attempts, first_failed_at, last_failed_at,
                  locked_until, notified_at, created_at, updated_at
           FROM login_attempt_counters WHERE scope = $1 AND key = $2
           FOR UPDATE"#,
        scope.as_str(),
        key,
    )
    .fetch_one(&mut *conn)
    .await
    .map_err(RepoError::from_sqlx)?;

    // A failure after a quiet window starts a new window: the
    // count resets to 1 and any (expired) lock clears. The
    // caller already checked the lock, so a live lock here can
    // only be the concurrent-race case (see the module docs) —
    // in that case the window cannot have expired (the last
    // failure was just now) and the lock extends below.
    let window_expired = row.last_failed_at < now - Duration::seconds(LOCKOUT_WINDOW_SECS);
    let new_attempts = if window_expired { 1 } else { row.attempts + 1 };
    let new_locked_until = match lock_seconds_for(i64::from(new_attempts)) {
        None => None,
        Some(secs) => {
            let fresh = now + Duration::seconds(secs);
            Some(match row.locked_until {
                Some(existing) if existing > fresh => existing,
                _ => fresh,
            })
        }
    };
    let newly_locked = !row.is_locked_at(now) && new_locked_until.is_some_and(|until| until > now);
    let first_failed_at = if window_expired {
        now
    } else {
        row.first_failed_at
    };
    let updated = sqlx::query_as!(
        Counter,
        r#"UPDATE login_attempt_counters
           SET attempts = $3, first_failed_at = $4, last_failed_at = $5,
               locked_until = $6, updated_at = $7
           WHERE scope = $1 AND key = $2
           RETURNING scope, key, attempts, first_failed_at, last_failed_at,
               locked_until, notified_at, created_at, updated_at"#,
        scope.as_str(),
        key,
        new_attempts,
        first_failed_at,
        now,
        new_locked_until,
        now,
    )
    .fetch_one(&mut *conn)
    .await
    .map_err(RepoError::from_sqlx)?;
    Ok((updated, newly_locked))
}

/// Mark a row as notified (the lockout mail was queued).
/// Called by the login service in the same transaction as the
/// enqueue, so the mail and the mark commit or roll back
/// together.
pub async fn mark_notified(
    conn: &mut PgConnection,
    scope: LockScope,
    key: &str,
    now: DateTime<Utc>,
) -> Result<(), RepoError> {
    sqlx::query!(
        r#"UPDATE login_attempt_counters SET notified_at = $3, updated_at = $3
           WHERE scope = $1 AND key = $2"#,
        scope.as_str(),
        key,
        now,
    )
    .execute(&mut *conn)
    .await
    .map_err(RepoError::from_sqlx)?;
    Ok(())
}

/// Delete a counter row (no error when absent). Called on a
/// successful login (both scopes) and on password reset (the
/// account scope — the reset-password ticket wires that call;
/// the function exists here so the hook is ready).
pub async fn reset(conn: &mut PgConnection, scope: LockScope, key: &str) -> Result<(), RepoError> {
    sqlx::query!(
        "DELETE FROM login_attempt_counters WHERE scope = $1 AND key = $2",
        scope.as_str(),
        key,
    )
    .execute(&mut *conn)
    .await
    .map_err(RepoError::from_sqlx)?;
    Ok(())
}
