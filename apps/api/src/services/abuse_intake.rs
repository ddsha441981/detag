//! Abuse intake service (P1-089, FR-C-06): `POST /v1/abuse`.
//!
//! Flow: honeypot first — a filled honeypot field drops the
//! submission before ANY work (no rate-limit token spent, no
//! DB write, no audit row, no mail), and the handler answers
//! the same 201 shape with a random id, so a bot cannot tell
//! a dropped submission from a filed one. Then the per-IP
//! rate limit (P1-089's `abuse-intake` bucket, keyed by the
//! sha256 of the client IP — the reporter has no account, so
//! there is no per-email key like register's), then one
//! transaction: the `abuse_reports` row + the
//! `abuse.report_filed` audit row + the `abuse-report-ack`
//! outbox enqueue. The transaction is the atomicity: no
//! report without its ack queued, and no ack for a report
//! that was not filed.
//!
//! Rate limit (P1-089): the `abuse-intake` bucket (5 burst /
//! 5 per min by default). A denial is a 429 `PF-3007` with
//! `Retry-After` and the bucket's `X-RateLimit-*` trio — the
//! error carries the bucket's burst so the handler stamps
//! the right `X-RateLimit-Limit`. Redis down fails OPEN
//! (warn logged), the P0-103 decision. An `"unknown"` client
//! IP (oneshot tests have no socket address) skips the
//! bucket — there is no identity to limit, the login
//! precedent — and production always has a socket address
//! (main.rs wires `ConnectInfo`).
//!
//! Privacy (NFR-42): the audit `after` payload carries the
//! reporter's email sha256, never the address. The outbox
//! payload is `{report_id}` only — the drain resolves the
//! address from the report row at send time (the mailer's
//! `abuse-report-ack` resolution path) and verifies its
//! sha256 against `to_hash`. The `reporter_contact` column
//! itself holds the raw address: it is the business record
//! (the queue needs to reply to the reporter), and the
//! data model names the column for exactly this purpose.
//!
//! The reported job IS checked for existence: a filing naming
//! a job id that does not exist is 404 `PF-2010` (the P1-008 /
//! P1-081 precedent for "named id does not exist"). The check
//! runs before the transaction — the `abuse_reports.job_id`
//! foreign key would reject the insert anyway (an FK
//! violation surfacing as a 500 would be a lie: the caller's
//! id is wrong, not the server). The FK is the backstop, not
//! the check. A deleted job unlinks its reports instead of
//! deleting them (`ON DELETE SET NULL`) — the report survives
//! with `job_id` NULL. (ASSUMPTION-050)

use chrono::{DateTime, Utc};
use detag_core::{
    audit::{record, NewAuditLog},
    checksum::sha256_hex,
    db::PoolConfig,
    redis::ratelimit::RateLimit,
    repos::{
        abuse_reports::{self, AbuseReport},
        email_outbox::{self, NewOutboxMail},
        jobs, RepoError,
    },
    tx::{self, TxError},
};
use detag_validator::{validate_email, validate_reason, Validate, ValidationErrors};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::rate_limit::RateLimitScope;
use crate::AppState;

/// The honeypot field: a plausible-looking field a human
/// never sees (the form hides it with CSS) and a bot fills.
/// Any non-blank value drops the submission.
pub const HONEYPOT_FIELD: &str = "website";

/// The ack template enqueued for a filed report.
pub const ABUSE_REPORT_ACK_TEMPLATE: &str = "abuse-report-ack";

/// The audit action for a filed report.
pub const REPORT_FILED_AUDIT_ACTION: &str = "abuse.report_filed";

/// The audit `actor_client` for intake writes. No user acts
/// here — the reporter is anonymous, the intake is the actor.
pub const INTAKE_CLIENT: &str = "abuse-intake";

/// The abuse report intake request body (api-spec: job id,
/// reporter contact, reason → 201 ticket).
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct AbuseIntakeRequest {
    /// The reported job's id, when the reporter knows it
    /// (`None` = a general-abuse report, no job).
    pub job_id: Option<Uuid>,
    /// The reporter's contact email (where the ack goes).
    pub reporter_contact: String,
    /// What happened (1–500 chars, the P1-008 reason shape).
    pub reason: String,
    /// Honeypot field — must stay blank. A human never sees
    /// this field; a bot fills it and is dropped silently.
    #[serde(default)]
    pub website: Option<String>,
}

impl AbuseIntakeRequest {
    /// The contact as stored: trimmed, lowercased. The
    /// address is case-insensitive for delivery, and storing
    /// the normalized form keeps the column tidy.
    pub fn normalized_contact(&self) -> String {
        self.reporter_contact.trim().to_lowercase()
    }
}

impl Validate for AbuseIntakeRequest {
    /// Structural validation only (no settings): contact email
    /// shape on the *normalized* contact (a pasted address
    /// with surrounding spaces is a typo, not an invalid
    /// address), reason 1–500 chars (the P1-008 shape). The
    /// honeypot is NOT validated — any value in it is a drop,
    /// never a 422.
    fn validate(&self) -> Result<(), ValidationErrors> {
        let mut errors = ValidationErrors::default();
        if let Err(err) = validate_email("reporter_contact", &self.normalized_contact()) {
            errors.push(err);
        }
        if let Err(err) = validate_reason("reason", &self.reason) {
            errors.push(err);
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

/// The filed report: the ticket the reporter holds (the ack
/// mail carries the same id).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AbuseReportResponse {
    pub id: Uuid,
    pub status: String,
    pub created_at: DateTime<Utc>,
}

impl From<AbuseReport> for AbuseReportResponse {
    fn from(report: AbuseReport) -> Self {
        Self {
            id: report.id,
            status: report.status,
            created_at: report.created_at,
        }
    }
}

/// What filing an abuse report can fail with. `Internal`
/// carries no detail by design — the service logs the source
/// (which never contains PII: no contact, no reason) and the
/// handler renders the generic 500.
#[derive(Debug, Error)]
pub enum AbuseIntakeError {
    #[error("too many abuse reports from this address")]
    RateLimited(RateLimit, u64),
    #[error("the reported job does not exist")]
    UnknownJob,
    #[error("abuse intake failed")]
    Internal,
}

/// What one intake did: filed (the report to render), or
/// dropped by the honeypot (the handler renders the same
/// 201 shape with a random id — no row, no audit, no mail).
#[derive(Debug, Clone)]
pub enum AbuseIntakeOutcome {
    Filed(AbuseReportResponse),
    HoneypotDropped,
}

/// The transaction closure's error: small on purpose — it must
/// not recurse into [`AbuseIntakeError`].
#[derive(Debug)]
enum AbuseIntakeTxError {
    Db(RepoError),
}

/// True when the honeypot field carries a bot's mark: any
/// non-blank value. `None`, empty, and whitespace-only are
/// all clean — a human never sees the field, so nothing a
/// human submits can trip it.
pub fn is_honeypot_filled(website: Option<&str>) -> bool {
    website.is_some_and(|value| !value.trim().is_empty())
}

/// File an abuse report. See the module docs for the flow,
/// the honeypot rule, and the privacy rules.
pub async fn file_abuse_report(
    state: &AppState,
    req: &AbuseIntakeRequest,
    client_ip: &str,
) -> Result<AbuseIntakeOutcome, AbuseIntakeError> {
    // Honeypot FIRST — before the rate limit, before any DB
    // work: a bot that fills the field does no work at all
    // and spends no bucket token. Silent by design: the
    // handler answers the same 201 shape, so the drop is
    // indistinguishable from a filing.
    if is_honeypot_filled(req.website.as_deref()) {
        tracing::debug!("abuse intake: honeypot filled, dropping submission");
        return Ok(AbuseIntakeOutcome::HoneypotDropped);
    }

    // Per-IP rate limit (P1-089), before the transaction: a
    // denied request does no work at all. Keyed by the sha256
    // of the client IP (pre-auth — the reporter has no
    // account). Fail open on a Redis error (P0-103): warn and
    // continue. `"unknown"` skips the bucket (the login
    // precedent — no identity to limit).
    if client_ip != "unknown" {
        let ip_hash = sha256_hex(client_ip.as_bytes());
        let bucket_check = state
            .rate_limit
            .abuse_intake
            .check(
                &state.redis,
                RateLimitScope::AbuseIntake.key(),
                &ip_hash,
                1.0,
            )
            .await;
        match bucket_check {
            Ok(result) if !result.allowed => {
                return Err(AbuseIntakeError::RateLimited(
                    result,
                    state.rate_limit.abuse_intake_burst(),
                ));
            }
            Ok(_) => {}
            Err(err) => {
                tracing::warn!(scope = "abuse-intake", error = %err, "rate limiter failing open: redis unavailable");
            }
        }
    }

    let contact = req.normalized_contact();
    let contact_hash = sha256_hex(contact.as_bytes());
    let reason = validate_reason("reason", &req.reason).map_err(|_| AbuseIntakeError::Internal)?;
    let job_id = req.job_id;

    // The job must exist when named (ASSUMPTION-050): the
    // FK would reject the insert, and a 500 for a wrong id
    // would be a lie. A read, not a lock — the job cannot be
    // deleted between this read and the insert in a way that
    // breaks the filing: the FK's `ON DELETE SET NULL` turns
    // a concurrent delete into a `job_id` NULL, and the
    // report still files.
    if let Some(job_id) = job_id {
        match jobs::by_id(&state.pool, job_id).await {
            Ok(Some(_)) => {}
            Ok(None) => return Err(AbuseIntakeError::UnknownJob),
            Err(db_err) => {
                tracing::error!(error = ?db_err, "abuse intake: job lookup failed");
                return Err(AbuseIntakeError::Internal);
            }
        }
    }

    let timeout = PoolConfig::default().transaction_timeout;

    // One transaction: the report row + the audit row + the
    // ack enqueue. The ack payload is `{report_id}` only —
    // the drain resolves the address from the report row.
    let report = tx::with_tx(&state.pool, timeout, |conn| {
        let contact = contact.clone();
        let contact_hash = contact_hash.clone();
        let reason = reason.clone();
        Box::pin(async move {
            let report = abuse_reports::insert_intake(&mut *conn, job_id, &contact, &reason)
                .await
                .map_err(AbuseIntakeTxError::Db)?;
            record(
                &mut *conn,
                &NewAuditLog {
                    actor_user_id: None,
                    actor_client: Some(INTAKE_CLIENT.to_string()),
                    action: REPORT_FILED_AUDIT_ACTION.to_string(),
                    target_type: "abuse_report".to_string(),
                    target_id: Some(report.id.to_string()),
                    before: None,
                    after: Some(serde_json::json!({
                        "job_id": job_id,
                        "email_hash": contact_hash,
                    })),
                    reason: Some(reason.clone()),
                },
            )
            .await
            .map_err(AbuseIntakeTxError::Db)?;
            email_outbox::enqueue(
                &mut *conn,
                &NewOutboxMail {
                    to_hash: contact_hash,
                    template: ABUSE_REPORT_ACK_TEMPLATE.to_string(),
                    payload: serde_json::json!({ "report_id": report.id }),
                    scheduled_at: None,
                },
            )
            .await
            .map_err(AbuseIntakeTxError::Db)?;
            Ok(report)
        })
    })
    .await
    .map_err(|err| match err {
        TxError::Inner(AbuseIntakeTxError::Db(db_err)) => {
            tracing::error!(error = ?db_err, "abuse intake: transaction failed (db)");
            AbuseIntakeError::Internal
        }
        other => {
            tracing::error!(error = ?other, "abuse intake: transaction failed");
            AbuseIntakeError::Internal
        }
    })?;
    Ok(AbuseIntakeOutcome::Filed(AbuseReportResponse::from(report)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn honeypot_is_blank_only_when_empty() {
        assert!(!is_honeypot_filled(None));
        assert!(!is_honeypot_filled(Some("")));
        assert!(!is_honeypot_filled(Some("   ")));
        assert!(is_honeypot_filled(Some("https://spam.example")));
        assert!(is_honeypot_filled(Some("x")));
    }

    #[test]
    fn validate_collects_contact_and_reason_failures() {
        let req = AbuseIntakeRequest {
            job_id: None,
            reporter_contact: "not-an-email".to_string(),
            reason: "   ".to_string(),
            website: None,
        };
        let err = req.validate().expect_err("both fields fail");
        assert_eq!(err.len(), 2);
        assert_eq!(err.0[0].field, "reporter_contact");
        assert_eq!(err.0[1].field, "reason");
    }

    #[test]
    fn validate_accepts_a_good_request_and_normalizes_the_contact() {
        let req = AbuseIntakeRequest {
            job_id: Some(Uuid::now_v7()),
            reporter_contact: "  Reporter@Example.com ".to_string(),
            reason: "Stolen artwork, please review.".to_string(),
            website: None,
        };
        assert!(req.validate().is_ok());
        assert_eq!(req.normalized_contact(), "reporter@example.com");
    }

    #[test]
    fn validate_rejects_an_overlong_reason() {
        let req = AbuseIntakeRequest {
            job_id: None,
            reporter_contact: "reporter@example.com".to_string(),
            reason: "a".repeat(501),
            website: None,
        };
        let err = req.validate().expect_err("501 chars is too long");
        assert_eq!(err.len(), 1);
        assert_eq!(err.0[0].field, "reason");
    }

    #[test]
    fn honeypot_value_is_never_a_validation_failure() {
        // A filled honeypot is a drop, never a 422 — the
        // request validates fine whatever the field holds.
        let req = AbuseIntakeRequest {
            job_id: None,
            reporter_contact: "reporter@example.com".to_string(),
            reason: "Spam.".to_string(),
            website: Some("https://spam.example".to_string()),
        };
        assert!(req.validate().is_ok());
    }
}
