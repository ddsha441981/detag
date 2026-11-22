//! Abuse queue + actions (P1-090, FR-C-06): `GET
//! /v1/admin/abuse-reports` and `POST
//! /v1/admin/abuse-reports/{id}/resolve` — the read side of
//! the queue the P1-089 intake files into. Until this ticket
//! the intake filed reports nobody could read (the admin
//! read/resolve routes were stubs); now an admin lists the
//! queue and acts on it.
//!
//! The queue has three states: `open` (filed, untouched),
//! `reviewing` (an admin picked it up), `resolved` (an
//! action was taken). The resolve actions are the spec's
//! three terminal ones — `reject_job`, `suspend_user`,
//! `dismiss` — plus `start_review`, the pickup transition
//! that makes the middle state reachable at all: without it
//! no report could ever become `reviewing` (the spec names
//! the three states but only the three terminal actions, so
//! the fourth action is the deviation — ADR-0054).
//!
//! Every action is audited (the AC), in the same transaction
//! as the mutation — the P1-008 atomicity: a resolve writes
//! `admin.abuse_reports.resolved` (actor = the admin,
//! target the report, `after.action` naming the action —
//! the audit action the matrix already promised for this
//! route, `authz.rs`), and the `suspend_user` effect
//! additionally writes `user.suspended` (the P1-008 shape:
//! the suspension is a privileged action of its own, and
//! the P1-092 matrix suite checks it by that name — a
//! suspension visible only inside the resolution row would
//! be invisible to the `user.suspended` filter). The
//! `start_review` pickup audits as `abuse.report_reviewing`
//! (it resolves nothing, so the resolved action name would
//! be a lie). The handler marks `AuditHandled` on every 200
//! so the P1-046 middleware never writes a second row —
//! including the idempotent no-ops, which deliberately
//! write none.
//!
//! Idempotency (the P1-008 / P1-071 precedent): resolving
//! an already-resolved report is a 200 with the report as
//! it stands — no write, no audit; starting review on an
//! already-reviewing report is the same. A concurrent
//! resolve is decided by the conditional UPDATE inside the
//! transaction — and the resolve runs FIRST, before the
//! action's effect, so a loser writes nothing at all: a
//! loser that flipped the job or suspended the user first
//! and then lost the resolve would commit that effect under
//! a report resolved (by the winner) as something else,
//! with no resolution audit row naming it (the 128th's
//! adoption-review fix). A terminal action on a report
//! that cannot take it is 422 `PF-2002`
//! (`reject_job` needs a `job_id`, `suspend_user` needs a
//! `user_id` — a general-abuse report has neither; a spike
//! report has a user but no job). An unknown report id is
//! 404 `PF-2010`.
//!
//! SLA (FR-C-06): the review target is 72 h from filing.
//! Every queue item carries `sla_due_at` (`created_at` +
//! 72 h) and `sla_breached` — a resolved report breached
//! when its `resolved_at` is past the due time, an open or
//! reviewing one when the clock is past it. The 72 h is a
//! target, not a contractual SLA (ADR-0053's honesty note).

use chrono::{DateTime, Duration, Utc};
use detag_core::{
    audit::{record, NewAuditLog},
    db::PoolConfig,
    pagination::{clamp_limit, Cursor, Page},
    repos::{
        abuse_reports::{self, AbuseReport},
        jobs, sessions,
        users::{self, User},
        RepoError,
    },
    tx::{self, TxError},
};
use detag_validator::{validate_reason, Validate, ValidationErrors};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::AppState;

/// The review target: 72 h from filing (FR-C-06).
pub const SLA_TARGET_HOURS: i64 = 72;

/// The audit action for a terminal resolve (the matrix
/// already promised this name for the route).
pub const REPORT_RESOLVED_AUDIT_ACTION: &str = "admin.abuse_reports.resolved";

/// The audit action for the review pickup.
pub const REPORT_REVIEWING_AUDIT_ACTION: &str = "abuse.report_reviewing";

/// The audit action for a suspension done through the queue
/// (the P1-008 action — the resolution row is not the
/// suspension's audit).
pub const QUEUE_SUSPEND_AUDIT_ACTION: &str = "user.suspended";

/// The `resolution` strings the three terminal actions
/// stamp on the report.
pub const RESOLUTION_DISMISSED: &str = "dismissed";
pub const RESOLUTION_JOB_REJECTED: &str = "job_rejected";
pub const RESOLUTION_USER_SUSPENDED: &str = "user_suspended";

/// The job status a rejected job carries. `jobs.status` is
/// plain TEXT with no CHECK (migration 004) — `rejected` is
/// a terminal value the Phase 2/3 lifecycle never writes
/// back out of (a rejected job never processes).
pub const REJECTED_JOB_STATUS: &str = "rejected";

/// The SLA due time: filing + 72 h.
pub fn sla_due_at(created_at: DateTime<Utc>) -> DateTime<Utc> {
    created_at + Duration::hours(SLA_TARGET_HOURS)
}

/// Whether the report breached its 72 h target at `now`: a
/// resolved report breached when its resolution landed past
/// the due time; an open or reviewing one breaches when the
/// clock is past it.
pub fn sla_breached(report: &AbuseReport, now: DateTime<Utc>) -> bool {
    let due = sla_due_at(report.created_at);
    match report.resolved_at {
        Some(resolved_at) => resolved_at > due,
        None => now > due,
    }
}

/// One queue row as the API returns it: the stored fields,
/// verbatim, plus the SLA pair. `reporter_contact` is the
/// business record (the queue needs to reply to the
/// reporter) — an admin/support read, never a public one.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct AbuseReportItem {
    /// The report's id.
    #[schema(value_type = String, example = "123e4567-e89b-42d3-a456-426614174109")]
    pub id: Uuid,
    /// The reported user, when the report names one (a
    /// detector spike always does; the public intake never
    /// does — the reporter has no account).
    #[schema(value_type = Option<String>, example = "123e4567-e89b-42d3-a456-426614174109")]
    pub user_id: Option<Uuid>,
    /// The reporter's contact email, as filed (intake
    /// reports only; detector reports carry `None`).
    pub reporter_contact: Option<String>,
    /// The reported job, when the report names one.
    #[schema(value_type = Option<String>, example = "123e4567-e89b-42d3-a456-426614174109")]
    pub job_id: Option<Uuid>,
    /// What happened, as filed or as the detector wrote it.
    pub reason: String,
    /// `open` | `reviewing` | `resolved`.
    pub status: String,
    /// What the resolution was (`dismissed` |
    /// `job_rejected` | `user_suspended`) — `None` until
    /// resolved.
    pub resolution: Option<String>,
    /// The resolving admin's notes — `None` until resolved
    /// (or until a resolve carries notes).
    pub notes: Option<String>,
    /// When the report was filed.
    pub created_at: DateTime<Utc>,
    /// When the row last changed (a review pickup bumps it
    /// too — it is not the resolution time).
    pub updated_at: DateTime<Utc>,
    /// When the report left the queue — `None` until
    /// resolved.
    pub resolved_at: Option<DateTime<Utc>>,
    /// The resolving admin — `None` until resolved.
    #[schema(value_type = Option<String>, example = "123e4567-e89b-42d3-a456-426614174109")]
    pub resolved_by: Option<Uuid>,
    /// The 72 h review target: `created_at` + 72 h.
    pub sla_due_at: DateTime<Utc>,
    /// Whether the 72 h target was missed (a resolved
    /// report: its `resolved_at` is past the due time; an
    /// open or reviewing one: the clock is past it).
    pub sla_breached: bool,
}

impl AbuseReportItem {
    /// Build the item from a row at `now` (the clock's now —
    /// the breach check for an unresolved report is a
    /// read-time comparison).
    pub fn from_row(row: AbuseReport, now: DateTime<Utc>) -> Self {
        let sla_due_at = sla_due_at(row.created_at);
        let sla_breached = sla_breached(&row, now);
        Self {
            id: row.id,
            user_id: row.user_id,
            reporter_contact: row.reporter_contact,
            job_id: row.job_id,
            reason: row.reason,
            status: row.status,
            resolution: row.resolution,
            notes: row.notes,
            created_at: row.created_at,
            updated_at: row.updated_at,
            resolved_at: row.resolved_at,
            resolved_by: row.resolved_by,
            sla_due_at,
            sla_breached,
        }
    }
}

/// One page of the queue: the api-spec pagination shape
/// (`{"items": [...], "next_cursor": "..."}` —
/// `next_cursor` omitted on the last page).
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct AbuseQueuePage {
    /// This page's reports, newest first (at most the
    /// requested limit).
    pub items: Vec<AbuseReportItem>,
    /// The cursor for the next page, or `None` when this
    /// is the last page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// The resolve action (P1-090): the three terminal actions
/// from the spec, plus the review pickup that makes the
/// `reviewing` state reachable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ResolveAction {
    /// Pick the report up: open → reviewing.
    StartReview,
    /// Reject the reported job (terminal): the job's status
    /// becomes `rejected`, the report resolves.
    RejectJob,
    /// Suspend the reported user (terminal): the full
    /// P1-008 sequence, the report resolves.
    SuspendUser,
    /// Drop the report (terminal): nothing happened, the
    /// report resolves.
    Dismiss,
}

/// The resolve body: the action, the admin's reason
/// (required — an admin action without one is not
/// auditable, the P1-008 precedent), and optional notes
/// stored on the report.
#[derive(Debug, Deserialize, ToSchema)]
pub struct ResolveAbuseReportRequest {
    /// What to do with the report.
    pub action: ResolveAction,
    /// Why the action is taken (1–500 characters after
    /// trimming; the audit carries it verbatim).
    pub reason: String,
    /// Notes for the report row (1–500 characters when
    /// present).
    #[serde(default)]
    pub notes: Option<String>,
}

impl ResolveAbuseReportRequest {
    /// The validated reason (trimmed) and notes (trimmed,
    /// when present).
    pub fn validated(&self) -> Result<(String, Option<String>), ValidationErrors> {
        let mut errors = ValidationErrors::default();
        let reason = match validate_reason("reason", &self.reason) {
            Ok(reason) => Some(reason),
            Err(err) => {
                errors.push(err);
                None
            }
        };
        let notes = match &self.notes {
            Some(notes) => match validate_reason("notes", notes) {
                Ok(notes) => Some(Some(notes)),
                Err(err) => {
                    errors.push(err);
                    None
                }
            },
            None => Some(None),
        };
        match (reason, notes) {
            (Some(reason), Some(notes)) => Ok((reason, notes)),
            _ => Err(errors),
        }
    }
}

impl Validate for ResolveAbuseReportRequest {
    fn validate(&self) -> Result<(), ValidationErrors> {
        self.validated().map(|_| ())
    }
}

/// What the queue can fail with. `Validation` carries the
/// field errors (a bad reason/notes — 422 with the field
/// list); the rest are single-code problems at the handler:
/// `NotFound` 404 `PF-2010`, the other three 422 `PF-2002`.
#[derive(Debug, Error)]
pub enum AbuseQueueError {
    /// The resolve body failed validation (reason/notes).
    #[error("abuse resolve validation failed")]
    Validation(ValidationErrors),
    /// No such report.
    #[error("abuse report not found")]
    NotFound,
    /// The `?status=` filter is not a queue state.
    #[error("unknown abuse queue status")]
    InvalidStatus,
    /// The cursor does not decode (or its `ts` is outside
    /// chrono's range — a tampered cursor, never a 500).
    #[error("invalid pagination cursor")]
    InvalidCursor,
    /// The action cannot apply to this report (reject a job
    /// the report does not name, suspend a user it does not
    /// name).
    #[error("action does not apply to this report")]
    ActionNotApplicable,
    /// A database read or write failed.
    #[error("abuse queue failed")]
    Internal,
}

/// The transaction closure's error: small on purpose — it must
/// not recurse into [`AbuseQueueError`].
#[derive(Debug)]
enum AbuseQueueTxError {
    Db(RepoError),
}

/// The outcome of a resolve: the report after the action
/// (or as it already was, on a no-op) + whether the call
/// changed nothing.
#[derive(Debug)]
pub struct ResolveOutcome {
    /// The report after the resolve (or as it already was,
    /// on a no-op).
    pub report: AbuseReportItem,
    /// The report was already in the target state — no
    /// write, no audit.
    pub already: bool,
}

/// The queue's states, for the `?status=` filter.
const QUEUE_STATUSES: [&str; 3] = ["open", "reviewing", "resolved"];

/// List one page of the queue: the optional status filter,
/// the optional user filter (P1-098 — the attestation
/// review queue links here per user; `None` lists every
/// report, an unknown user is an empty page, not a 404),
/// the clamped limit, the decoded cursor. Fetches `limit +
/// 1` rows so the page knows whether a next page exists.
/// An unknown `status` is 422 (a list filter that names no
/// state is a client error, not an empty page — the value
/// is an enum, not a free-form filter like the audit
/// `action`).
pub async fn list_abuse_queue(
    state: &AppState,
    status: Option<String>,
    user_id: Option<Uuid>,
    limit: Option<u32>,
    cursor: Option<String>,
) -> Result<AbuseQueuePage, AbuseQueueError> {
    if status
        .as_deref()
        .is_some_and(|value| !QUEUE_STATUSES.contains(&value))
    {
        return Err(AbuseQueueError::InvalidStatus);
    }
    let limit = clamp_limit(limit);
    let cursor = match cursor {
        Some(raw) => {
            let decoded = Cursor::decode(&raw).map_err(|_| AbuseQueueError::InvalidCursor)?;
            // The same out-of-range `ts` guard as the audit
            // query (P1-088): a decoded cursor can carry a
            // `ts` outside chrono's range, and the repo's
            // `from_timestamp_millis(...).expect(...)` would
            // panic on it — out of range is a tampered
            // cursor (422), never a 500.
            DateTime::from_timestamp_millis(decoded.ts).ok_or(AbuseQueueError::InvalidCursor)?;
            Some(decoded)
        }
        None => None,
    };
    let now = state.clock.now();
    let rows =
        abuse_reports::list_filtered(&state.pool, status.as_deref(), user_id, cursor, limit + 1)
            .await
            .map_err(|err| {
                tracing::error!(error = ?err, "abuse queue: list failed");
                AbuseQueueError::Internal
            })?;
    let page = Page::from_window(rows, limit, |row| Cursor {
        ts: row.created_at.timestamp_millis(),
        id: row.id,
    });
    Ok(AbuseQueuePage {
        items: page
            .items
            .into_iter()
            .map(|row| AbuseReportItem::from_row(row, now))
            .collect(),
        next_cursor: page.next_cursor,
    })
}

/// Resolve one report. See the module docs for the actions,
/// the audits, and the idempotency rules.
pub async fn resolve_abuse_report(
    state: &AppState,
    admin: &User,
    report_id: Uuid,
    req: &ResolveAbuseReportRequest,
) -> Result<ResolveOutcome, AbuseQueueError> {
    let (reason, notes) = req.validated().map_err(AbuseQueueError::Validation)?;
    let report = abuse_reports::by_id(&state.pool, report_id)
        .await
        .map_err(|err| {
            tracing::error!(error = ?err, "abuse queue: report lookup failed");
            AbuseQueueError::Internal
        })?
        .ok_or(AbuseQueueError::NotFound)?;
    // Already resolved: a 200 no-op (the P1-071 precedent)
    // — no write, no audit, whatever the action was.
    if report.status == "resolved" {
        return Ok(ResolveOutcome {
            report: AbuseReportItem::from_row(report, state.clock.now()),
            already: true,
        });
    }
    match req.action {
        ResolveAction::StartReview => start_review(state, admin, report, reason).await,
        ResolveAction::RejectJob => reject_job(state, admin, report, reason, notes).await,
        ResolveAction::SuspendUser => suspend_user(state, admin, report, reason, notes).await,
        ResolveAction::Dismiss => dismiss(state, admin, report, reason, notes).await,
    }
}

/// Pick a report up: open → reviewing, audited as
/// `abuse.report_reviewing`. Already reviewing is a 200
/// no-op. The conditional UPDATE is the race guard: two
/// admins picking up the same report at once — the loser
/// re-reads and answers the no-op.
async fn start_review(
    state: &AppState,
    admin: &User,
    report: AbuseReport,
    reason: String,
) -> Result<ResolveOutcome, AbuseQueueError> {
    if report.status == "reviewing" {
        return Ok(ResolveOutcome {
            report: AbuseReportItem::from_row(report, state.clock.now()),
            already: true,
        });
    }
    let timeout = PoolConfig::default().transaction_timeout;
    let updated = tx::with_tx(&state.pool, timeout, |conn| {
        let admin = admin.clone();
        let reason = reason.clone();
        let report = report.clone();
        Box::pin(async move {
            let updated = abuse_reports::mark_reviewing(&mut *conn, report.id)
                .await
                .map_err(AbuseQueueTxError::Db)?;
            if updated.is_none() {
                return Ok(None);
            }
            record(
                &mut *conn,
                &NewAuditLog {
                    actor_user_id: Some(admin.id),
                    actor_client: None,
                    action: REPORT_REVIEWING_AUDIT_ACTION.to_string(),
                    target_type: "abuse_report".to_string(),
                    target_id: Some(report.id.to_string()),
                    before: Some(serde_json::json!({ "status": report.status })),
                    after: Some(serde_json::json!({ "status": "reviewing" })),
                    reason: Some(reason),
                },
            )
            .await
            .map_err(AbuseQueueTxError::Db)?;
            Ok(updated)
        })
    })
    .await
    .map_err(map_tx_error("abuse queue: start review failed"))?;
    match updated {
        Some(row) => Ok(ResolveOutcome {
            report: AbuseReportItem::from_row(row, state.clock.now()),
            already: false,
        }),
        None => already_resolved(state, report.id).await,
    }
}

/// Reject the reported job: the job's status becomes
/// `rejected` and the report resolves as `job_rejected`,
/// one transaction, audited as
/// `admin.abuse_reports.resolved` with the job in `after`.
/// A report with no job cannot take this action (422).
/// The resolve runs FIRST, before the job flip: the
/// conditional UPDATE decides a concurrent resolve, and a
/// loser must write nothing — flipping the job first and
/// then losing the resolve would commit a rejected job
/// under a report resolved (by the winner) as something
/// else, with no resolution audit row naming the rejection.
async fn reject_job(
    state: &AppState,
    admin: &User,
    report: AbuseReport,
    reason: String,
    notes: Option<String>,
) -> Result<ResolveOutcome, AbuseQueueError> {
    let Some(job_id) = report.job_id else {
        return Err(AbuseQueueError::ActionNotApplicable);
    };
    let job = jobs::by_id(&state.pool, job_id).await.map_err(|err| {
        tracing::error!(error = ?err, "abuse queue: job lookup failed");
        AbuseQueueError::Internal
    })?;
    let Some(job) = job else {
        return Err(AbuseQueueError::NotFound);
    };
    let timeout = PoolConfig::default().transaction_timeout;
    let updated = tx::with_tx(&state.pool, timeout, |conn| {
        let admin = admin.clone();
        let reason = reason.clone();
        let notes = notes.clone();
        let report = report.clone();
        let job_status_before = job.status.clone();
        Box::pin(async move {
            let updated = abuse_reports::resolve(
                &mut *conn,
                report.id,
                RESOLUTION_JOB_REJECTED,
                notes.as_deref(),
                admin.id,
            )
            .await
            .map_err(AbuseQueueTxError::Db)?;
            let Some(updated) = updated else {
                // A concurrent resolve won: nothing was
                // written yet, so the transaction commits
                // empty and the caller re-reads the row.
                return Ok(None);
            };
            jobs::set_status(&mut *conn, job_id, REJECTED_JOB_STATUS)
                .await
                .map_err(AbuseQueueTxError::Db)?;
            record(
                &mut *conn,
                &NewAuditLog {
                    actor_user_id: Some(admin.id),
                    actor_client: None,
                    action: REPORT_RESOLVED_AUDIT_ACTION.to_string(),
                    target_type: "abuse_report".to_string(),
                    target_id: Some(report.id.to_string()),
                    before: Some(serde_json::json!({ "status": report.status })),
                    after: Some(serde_json::json!({
                        "action": "reject_job",
                        "resolution": RESOLUTION_JOB_REJECTED,
                        "job_id": job_id,
                        "job_status_before": job_status_before,
                        "job_status": REJECTED_JOB_STATUS,
                    })),
                    reason: Some(reason),
                },
            )
            .await
            .map_err(AbuseQueueTxError::Db)?;
            Ok(Some(updated))
        })
    })
    .await
    .map_err(map_tx_error("abuse queue: reject job failed"))?;
    match updated {
        Some(row) => Ok(ResolveOutcome {
            report: AbuseReportItem::from_row(row, state.clock.now()),
            already: false,
        }),
        None => already_resolved(state, report.id).await,
    }
}

/// Suspend the reported user: the full P1-008 sequence
/// (status flip + sessions revoked + queued jobs held,
/// audited as `user.suspended` with the admin as actor)
/// plus the report's resolution, one transaction. A report
/// with no user cannot take this action (422). An
/// already-suspended account is left alone — the P1-008
/// idempotency — but the report still resolves (its
/// resolution is the new state change), and the resolution
/// row says so. The resolve runs FIRST, before the
/// suspension: a loser that suspended first and then lost
/// the resolve would commit a suspended user (sessions
/// revoked, jobs held, `user.suspended` row written) under
/// a report resolved (by the winner) as something else,
/// with no resolution audit row naming the suspension.
async fn suspend_user(
    state: &AppState,
    admin: &User,
    report: AbuseReport,
    reason: String,
    notes: Option<String>,
) -> Result<ResolveOutcome, AbuseQueueError> {
    let Some(user_id) = report.user_id else {
        return Err(AbuseQueueError::ActionNotApplicable);
    };
    let target = users::by_id(&state.pool, user_id).await.map_err(|err| {
        tracing::error!(error = ?err, "abuse queue: user lookup failed");
        AbuseQueueError::Internal
    })?;
    let Some(target) = target else {
        return Err(AbuseQueueError::NotFound);
    };
    let timeout = PoolConfig::default().transaction_timeout;
    let updated = tx::with_tx(&state.pool, timeout, |conn| {
        let admin = admin.clone();
        let reason = reason.clone();
        let notes = notes.clone();
        let report = report.clone();
        let target = target.clone();
        Box::pin(async move {
            let updated = abuse_reports::resolve(
                &mut *conn,
                report.id,
                RESOLUTION_USER_SUSPENDED,
                notes.as_deref(),
                admin.id,
            )
            .await
            .map_err(AbuseQueueTxError::Db)?;
            let Some(updated) = updated else {
                // A concurrent resolve won: nothing was
                // written yet, so the transaction commits
                // empty and the caller re-reads the row.
                return Ok(None);
            };
            let mut sessions_revoked = 0;
            let mut jobs_held = 0;
            let already_suspended = target.status == "suspended";
            if !already_suspended {
                users::suspend(&mut *conn, target.id)
                    .await
                    .map_err(AbuseQueueTxError::Db)?;
                sessions_revoked = sessions::revoke_all_for_user(&mut *conn, target.id)
                    .await
                    .map_err(AbuseQueueTxError::Db)?;
                jobs_held = jobs::hold_queued_for_user(&mut *conn, target.id)
                    .await
                    .map_err(AbuseQueueTxError::Db)?;
                record(
                    &mut *conn,
                    &NewAuditLog {
                        actor_user_id: Some(admin.id),
                        actor_client: None,
                        action: QUEUE_SUSPEND_AUDIT_ACTION.to_string(),
                        target_type: "user".to_string(),
                        target_id: Some(target.id.to_string()),
                        before: Some(serde_json::json!({
                            "status": target.status,
                            "deleted_at": target.deleted_at,
                        })),
                        after: Some(serde_json::json!({
                            "status": "suspended",
                            "sessions_revoked": sessions_revoked,
                            "jobs_held": jobs_held,
                        })),
                        reason: Some(reason.clone()),
                    },
                )
                .await
                .map_err(AbuseQueueTxError::Db)?;
                // The ladder's second rung mails the user
                // (P1-091) — in the same transaction as the
                // suspension, and only on a real transition:
                // an already-suspended account already got
                // this mail for the suspension that put it
                // there.
                crate::services::enforcement::enqueue_step_mail(
                    &mut *conn,
                    &target,
                    crate::services::enforcement::ENFORCEMENT_SUSPENSION_TEMPLATE,
                    &reason,
                )
                .await
                .map_err(AbuseQueueTxError::Db)?;
            }
            record(
                &mut *conn,
                &NewAuditLog {
                    actor_user_id: Some(admin.id),
                    actor_client: None,
                    action: REPORT_RESOLVED_AUDIT_ACTION.to_string(),
                    target_type: "abuse_report".to_string(),
                    target_id: Some(report.id.to_string()),
                    before: Some(serde_json::json!({ "status": report.status })),
                    after: Some(serde_json::json!({
                        "action": "suspend_user",
                        "resolution": RESOLUTION_USER_SUSPENDED,
                        "user_id": user_id,
                        "already_suspended": already_suspended,
                        "sessions_revoked": sessions_revoked,
                        "jobs_held": jobs_held,
                    })),
                    reason: Some(reason),
                },
            )
            .await
            .map_err(AbuseQueueTxError::Db)?;
            Ok(Some(updated))
        })
    })
    .await
    .map_err(map_tx_error("abuse queue: suspend user failed"))?;
    match updated {
        Some(row) => Ok(ResolveOutcome {
            report: AbuseReportItem::from_row(row, state.clock.now()),
            already: false,
        }),
        None => already_resolved(state, report.id).await,
    }
}

/// Dismiss the report: no effect beyond the resolution,
/// audited as `admin.abuse_reports.resolved` with the
/// action in `after`.
async fn dismiss(
    state: &AppState,
    admin: &User,
    report: AbuseReport,
    reason: String,
    notes: Option<String>,
) -> Result<ResolveOutcome, AbuseQueueError> {
    let timeout = PoolConfig::default().transaction_timeout;
    let updated = tx::with_tx(&state.pool, timeout, |conn| {
        let admin = admin.clone();
        let reason = reason.clone();
        let notes = notes.clone();
        let report = report.clone();
        Box::pin(async move {
            let updated = abuse_reports::resolve(
                &mut *conn,
                report.id,
                RESOLUTION_DISMISSED,
                notes.as_deref(),
                admin.id,
            )
            .await
            .map_err(AbuseQueueTxError::Db)?;
            if updated.is_none() {
                return Ok(None);
            }
            record(
                &mut *conn,
                &NewAuditLog {
                    actor_user_id: Some(admin.id),
                    actor_client: None,
                    action: REPORT_RESOLVED_AUDIT_ACTION.to_string(),
                    target_type: "abuse_report".to_string(),
                    target_id: Some(report.id.to_string()),
                    before: Some(serde_json::json!({ "status": report.status })),
                    after: Some(serde_json::json!({
                        "action": "dismiss",
                        "resolution": RESOLUTION_DISMISSED,
                    })),
                    reason: Some(reason),
                },
            )
            .await
            .map_err(AbuseQueueTxError::Db)?;
            Ok(updated)
        })
    })
    .await
    .map_err(map_tx_error("abuse queue: dismiss failed"))?;
    match updated {
        Some(row) => Ok(ResolveOutcome {
            report: AbuseReportItem::from_row(row, state.clock.now()),
            already: false,
        }),
        None => already_resolved(state, report.id).await,
    }
}

/// The conditional UPDATE lost the race (a concurrent
/// resolve won it): re-read the row and answer the
/// idempotent 200 with its current state. The row exists
/// (the caller loaded it before the transaction), so a
/// missing re-read is an infrastructure failure, not a
/// 404.
async fn already_resolved(
    state: &AppState,
    report_id: Uuid,
) -> Result<ResolveOutcome, AbuseQueueError> {
    let report = abuse_reports::by_id(&state.pool, report_id)
        .await
        .map_err(|err| {
            tracing::error!(error = ?err, "abuse queue: re-read after race failed");
            AbuseQueueError::Internal
        })?
        .ok_or(AbuseQueueError::Internal)?;
    Ok(ResolveOutcome {
        report: AbuseReportItem::from_row(report, state.clock.now()),
        already: true,
    })
}

/// Map a transaction failure to the service error (the
/// P1-008 shape: a `NotFound` from a repo call inside the
/// transaction is a 404; anything else is a 500 — the
/// service logs the source, which never contains PII).
fn map_tx_error(context: &'static str) -> impl Fn(TxError<AbuseQueueTxError>) -> AbuseQueueError {
    move |err| match err {
        TxError::Inner(AbuseQueueTxError::Db(RepoError::NotFound)) => AbuseQueueError::NotFound,
        TxError::Inner(AbuseQueueTxError::Db(db_err)) => {
            tracing::error!(error = ?db_err, "{context} (db)");
            AbuseQueueError::Internal
        }
        other => {
            tracing::error!(error = ?other, "{context}");
            AbuseQueueError::Internal
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;

    fn report_at(
        status: &str,
        created_at: DateTime<Utc>,
        resolved_at: Option<DateTime<Utc>>,
    ) -> AbuseReport {
        AbuseReport {
            id: Uuid::now_v7(),
            user_id: None,
            reporter_contact: Some("reporter@example.com".to_string()),
            job_id: None,
            reason: "Stolen artwork.".to_string(),
            status: status.to_string(),
            resolution: resolved_at.map(|_| "dismissed".to_string()),
            notes: None,
            created_at,
            updated_at: created_at,
            resolved_at,
            resolved_by: None,
        }
    }

    #[test]
    fn sla_due_is_filing_plus_72h() {
        let filed = Utc.with_ymd_and_hms(2026, 11, 19, 12, 0, 0).unwrap();
        assert_eq!(
            sla_due_at(filed),
            Utc.with_ymd_and_hms(2026, 11, 22, 12, 0, 0).unwrap()
        );
    }

    #[test]
    fn sla_breach_covers_all_four_cases() {
        let filed = Utc.with_ymd_and_hms(2026, 11, 19, 12, 0, 0).unwrap();
        let due = sla_due_at(filed);
        // Open, fresh: no breach.
        let open = report_at("open", filed, None);
        assert!(!sla_breached(&open, filed));
        // Open, past the due time: breached.
        assert!(sla_breached(&open, due + Duration::seconds(1)));
        // Resolved on time: no breach, even read late.
        let on_time = report_at("resolved", filed, Some(due - Duration::hours(1)));
        assert!(!sla_breached(&on_time, due + Duration::days(30)));
        // Resolved late: breached, however early the read.
        let late = report_at("resolved", filed, Some(due + Duration::seconds(1)));
        assert!(sla_breached(&late, filed));
    }

    #[test]
    fn item_carries_the_sla_pair() {
        let filed = Utc.with_ymd_and_hms(2026, 11, 19, 12, 0, 0).unwrap();
        let row = report_at("open", filed, None);
        let item = AbuseReportItem::from_row(row, filed);
        assert_eq!(item.sla_due_at, sla_due_at(filed));
        assert!(!item.sla_breached);
        let json = serde_json::to_value(&item).expect("serializes");
        assert_eq!(json["status"], "open");
        assert_eq!(json["sla_due_at"], "2026-11-22T12:00:00Z");
        assert_eq!(json["sla_breached"], false);
        assert!(json["resolved_at"].is_null(), "unresolved: {json}");
    }

    #[test]
    fn actions_deserialize_by_snake_case_name() {
        let dismiss: ResolveAction = serde_json::from_value(json!("dismiss")).unwrap();
        assert_eq!(dismiss, ResolveAction::Dismiss);
        let review: ResolveAction = serde_json::from_value(json!("start_review")).unwrap();
        assert_eq!(review, ResolveAction::StartReview);
        assert!(serde_json::from_value::<ResolveAction>(json!("nuke")).is_err());
    }

    #[test]
    fn validate_collects_reason_and_notes_failures() {
        let req = ResolveAbuseReportRequest {
            action: ResolveAction::Dismiss,
            reason: "   ".to_string(),
            notes: Some("x".repeat(501)),
        };
        let err = req.validate().expect_err("both fields fail");
        assert_eq!(err.len(), 2);
        assert_eq!(err.0[0].field, "reason");
        assert_eq!(err.0[1].field, "notes");
    }

    #[test]
    fn validate_accepts_a_good_request_and_trims() {
        let req = ResolveAbuseReportRequest {
            action: ResolveAction::RejectJob,
            reason: "  Confirmed infringement.  ".to_string(),
            notes: Some("  See the takedown ticket. ".to_string()),
        };
        let (reason, notes) = req.validated().expect("valid");
        assert_eq!(reason, "Confirmed infringement.");
        assert_eq!(notes.as_deref(), Some("See the takedown ticket."));
    }
}
