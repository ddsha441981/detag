//! Email-outbox repository (P1-001, E1.1). `email_outbox` is the
//! durable queue between the api and the mail drain (migration
//! 004, data-model doc): the api enqueues, the worker drains
//! unsent rows due now (`sent_at IS NULL`, partial index).
//!
//! Privacy (NFR-42): `to_hash` is the sha256 of the recipient
//! (the drain resolves the address from the users table at send
//! time); the payload carries ids, never a raw email address.
//! The template name is the contract with the mailer
//! (`verify-email`, ...); the payload shape is the template's
//! own — `verify-email` carries `{user_id, token}` (P1-002:
//! the token is minted with the user row, in the same
//! transaction that enqueues this row — one mint, one row).

use super::error::RepoError;
use chrono::{DateTime, Utc};
use serde_json::Value;
use uuid::Uuid;

/// One mail to enqueue.
#[derive(Debug, Clone)]
pub struct NewOutboxMail {
    /// sha256 of the recipient address (never the address itself).
    pub to_hash: String,
    /// Template name, e.g. `verify-email`.
    pub template: String,
    /// Template payload (ids only, no PII).
    pub payload: Value,
    /// When the mail becomes due (`None` = now).
    pub scheduled_at: Option<DateTime<Utc>>,
}

/// A stored outbox row (the fields tests and the drain need).
#[derive(Debug, Clone, PartialEq)]
pub struct OutboxMail {
    pub id: Uuid,
    pub to_hash: Option<String>,
    pub template: String,
    pub payload: Value,
    pub scheduled_at: DateTime<Utc>,
    pub sent_at: Option<DateTime<Utc>>,
    pub attempts: i32,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Enqueue a mail. Returns the row's id.
pub async fn enqueue(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    mail: &NewOutboxMail,
) -> Result<Uuid, RepoError> {
    let scheduled_at = mail.scheduled_at.unwrap_or_else(Utc::now);
    let row = sqlx::query!(
        r#"INSERT INTO email_outbox (to_hash, template, payload, scheduled_at)
           VALUES ($1, $2, $3, $4)
           RETURNING id"#,
        mail.to_hash,
        mail.template,
        mail.payload,
        scheduled_at,
    )
    .fetch_one(pool)
    .await
    .map_err(RepoError::from_sqlx)?;
    Ok(row.id)
}

/// Read back the unsent rows for one template (oldest first).
/// The register test uses it to prove the verification mail was
/// queued; the worker drain builds on it (with its own due-now
/// + locking query — this one is a plain read, not a claim).
pub async fn unsent_for_template(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    template: &str,
) -> Result<Vec<OutboxMail>, RepoError> {
    sqlx::query_as!(
        OutboxMail,
        r#"SELECT id, to_hash, template, payload, scheduled_at,
                  sent_at, attempts, last_error, created_at
           FROM email_outbox
           WHERE template = $1 AND sent_at IS NULL
           ORDER BY scheduled_at ASC, id ASC"#,
        template,
    )
    .fetch_all(pool)
    .await
    .map_err(RepoError::from_sqlx)
}

/// Claim up to `limit` due rows for sending (P1-028, the mail
/// drain). One atomic statement: the claim bumps `attempts` and
/// returns the rows, so two drain replicas can never send the
/// same mail — the inner `FOR UPDATE SKIP LOCKED` skips rows
/// another claimer already holds. A row is due when it is
/// unsent, its `scheduled_at` has passed, and it has attempts
/// left (`attempts < max_attempts`); a row that exhausted its
/// attempts is terminal — it stays in the table with
/// `last_error` set and is never claimed again (the drain's
/// `record_permanent_failure` also parks unfixable rows here by
/// setting `attempts = max_attempts`). `now` is the caller's
/// clock (the drain passes its own `Utc::now()`, so production
/// behaviour is identical to the database clock within
/// milliseconds) — threading it through, instead of the
/// database's `now()`, keeps the drain's backoff schedule and
/// its claim in the same time frame, which is what makes the
/// retry timing unit-testable with a simulated clock.
pub async fn claim_due(
    pool: &sqlx::PgPool,
    limit: i64,
    max_attempts: i32,
    now: DateTime<Utc>,
) -> Result<Vec<OutboxMail>, RepoError> {
    sqlx::query_as!(
        OutboxMail,
        r#"UPDATE email_outbox
           SET attempts = attempts + 1
           WHERE id IN (
               SELECT id FROM email_outbox
               WHERE sent_at IS NULL
                 AND scheduled_at <= $3
                 AND attempts < $2
               ORDER BY scheduled_at ASC, id ASC
               LIMIT $1
               FOR UPDATE SKIP LOCKED
           )
           RETURNING id, to_hash, template, payload, scheduled_at,
                     sent_at, attempts, last_error, created_at"#,
        limit,
        max_attempts,
        now,
    )
    .fetch_all(pool)
    .await
    .map_err(RepoError::from_sqlx)
}

/// Mark a row sent: `sent_at` is now, the error (if any) clears.
/// A sent row is never claimed again; retention purges it after
/// 30 days (data-model doc).
pub async fn mark_sent(pool: &sqlx::PgPool, id: Uuid) -> Result<(), RepoError> {
    sqlx::query!(
        r#"UPDATE email_outbox
           SET sent_at = now(), last_error = NULL
           WHERE id = $1"#,
        id,
    )
    .execute(pool)
    .await
    .map_err(RepoError::from_sqlx)?;
    Ok(())
}

/// Record a transient send failure (P1-028): the next attempt is
/// scheduled at `next_attempt_at` (the drain's exponential
/// backoff) and the error is kept, truncated — the raw provider
/// error can be long and must never carry PII into the row
/// (the drain only ever passes its own short error strings).
pub async fn record_failure(
    pool: &sqlx::PgPool,
    id: Uuid,
    error: &str,
    next_attempt_at: DateTime<Utc>,
) -> Result<(), RepoError> {
    sqlx::query!(
        r#"UPDATE email_outbox
           SET scheduled_at = $2, last_error = $3
           WHERE id = $1"#,
        id,
        next_attempt_at,
        truncate_error(error),
    )
    .execute(pool)
    .await
    .map_err(RepoError::from_sqlx)?;
    Ok(())
}

/// Park a row that can never be sent (P1-028): unknown template,
/// unresolvable recipient, or a `to_hash` that does not match
/// the resolved address. Sets `attempts = max_attempts` so the
/// claim filter never picks it again; `last_error` keeps the
/// reason for ops. Retrying a permanent failure is pointless —
/// the data, not the network, is wrong.
pub async fn record_permanent_failure(
    pool: &sqlx::PgPool,
    id: Uuid,
    max_attempts: i32,
    error: &str,
) -> Result<(), RepoError> {
    sqlx::query!(
        r#"UPDATE email_outbox
           SET attempts = $2, last_error = $3
           WHERE id = $1"#,
        id,
        max_attempts,
        truncate_error(error),
    )
    .execute(pool)
    .await
    .map_err(RepoError::from_sqlx)?;
    Ok(())
}

/// `last_error` is a short ops note, not a log dump: 500 chars
/// max, cut on a char boundary.
fn truncate_error(error: &str) -> String {
    const MAX_LEN: usize = 500;
    if error.len() <= MAX_LEN {
        error.to_string()
    } else {
        let mut cut = MAX_LEN;
        while !error.is_char_boundary(cut) {
            cut -= 1;
        }
        format!("{}…", &error[..cut])
    }
}

/// Count mails queued for one user + template in the last
/// `window_secs` seconds (sent or unsent — the mailer marks
/// rows sent, it never deletes them).
///
/// P1-002: this is the resend-rate-limit counter. It lives
/// here, not on the token table, on purpose: minting deletes
/// the user's previous unused tokens (one live token per
/// user), so the token table cannot count history — but every
/// mint enqueues exactly one outbox row in the same
/// transaction, so the outbox count IS the mint count, 1:1.
/// (The token repo's `count_recent_for_user` counted rows
/// that minting deletes; it was removed — see the P1-002
/// status — rather than left to silently under-count.)
pub async fn count_recent_for_user(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    template: &str,
    user_id: Uuid,
    window_secs: i64,
) -> Result<i64, RepoError> {
    let row = sqlx::query!(
        r#"SELECT COUNT(*) AS "count!"
           FROM email_outbox
           WHERE template = $1
             AND payload ->> 'user_id' = $2
             AND created_at >= now() - make_interval(secs => $3)"#,
        template,
        user_id.to_string(),
        window_secs as f64,
    )
    .fetch_one(pool)
    .await
    .map_err(RepoError::from_sqlx)?;
    Ok(row.count)
}
