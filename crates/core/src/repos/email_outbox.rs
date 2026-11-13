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
//! own — `verify-email` carries `{user_id}` until P1-002 mints
//! the verification token (the token is P1-002's scope, not
//! this ticket's).

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
