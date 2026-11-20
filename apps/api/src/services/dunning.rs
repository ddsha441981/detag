//! Dunning policy job (P1-070, FR-P-03): the past_due
//! sequence after day 0.
//!
//! Day 0 IS the `subscription-dunning` mail the state
//! machine sends on the move to `past_due` (P1-064,
//! ADR-0035) — this job never sends one, so a subscription
//! can never be double-dunned on the day it fails. The job
//! handles the rest of the sequence:
//!
//! * day 3 — the `subscription-dunning-day3` reminder;
//! * day 7 — the `subscription-dunning-day7` reminder;
//! * day 14 — the subscription is canceled
//!   (`past_due → canceled` through the state machine, so
//!   the `subscription-canceled` mail and the
//!   `billing.subscription.status_changed` audit land with
//!   it). Day 14 suspends the SUBSCRIPTION, never the user
//!   account (ADR-0035): canceling revokes paid
//!   entitlements immediately via `effective_plan` (P1-049),
//!   and the user keeps their account and can re-subscribe.
//!
//! # How a row is identified
//!
//! The `subscriptions` row carries its own dunning state
//! (migration 20261118043000): `past_due_since` — stamped
//! on the move INTO `past_due` from the app clock, cleared
//! on any move OUT of it — and `dunning_step` — the last
//! reminder step mailed (0/3/7). Days past due are whole
//! days since `past_due_since`, counted from the FIRST
//! failure (a second failed invoice while already past due
//! is a same-state no-op and never re-stamps it). The audit
//! trail stays the trail: it is not a query index.
//!
//! # Exactly-once
//!
//! A reminder lands in one transaction with its guarded
//! step bump (`dunning_step = $from → $to`, the row's step
//! as the guard) and its `billing.subscription
//! .dunning_reminder` audit row — a re-run the same day
//! sees the new step and skips the mail, and a concurrent
//! run loses the guard and re-reads instead of mailing
//! twice. A row whose step is already past a day never
//! gets that day's mail, even if the job was down when the
//! day passed — the next step (or the cancel) still fires
//! on its own day.
//!
//! # The retry window
//!
//! The provider retries the failed charge on its own
//! schedule (Stripe Smart Retries — we neither control nor
//! observe it); this job NEVER re-attempts a charge and
//! makes NO provider call at all. "Retry window" is the
//! healing window: a successful payment before day 14
//! (`invoice.paid` / status → `active`) clears the dunning
//! state and stops all further reminders. After day 14 the
//! row is canceled and a later payment does NOT resurrect
//! it — the user checks out again (a new row).
//!
//! # Invocation
//!
//! [`run_dunning`] is the invocable job: the nightly
//! schedule that enqueues it is P1-082's (the worker's
//! `dunning` kind is a stub until then — the worker has no
//! pool/clock yet, the catalog_sync precedent). The job
//! reads its policy from `state.settings`
//! (`PF_BILLING_DUNNING_*`); `false` on
//! `PF_BILLING_DUNNING_ENABLED` pauses it without touching
//! the schedule. One row's failure never aborts the sweep:
//! it is logged + counted and the job continues (the P1-061
//! per-plan-failure precedent).

use crate::AppState;
use detag_core::{
    audit::{record, NewAuditLog},
    checksum::sha256_hex,
    config::Settings,
    db::PoolConfig,
    error::AppError,
    repos::{
        email_outbox::{self, NewOutboxMail},
        subscriptions::{self, Subscription},
        users,
    },
    tx::{self, TxError},
};

/// The audit action for a dunning reminder mail.
pub const DUNNING_REMINDER_ACTION: &str = "billing.subscription.dunning_reminder";
/// The day-3 reminder template.
pub const DUNNING_DAY3_TEMPLATE: &str = "subscription-dunning-day3";
/// The day-7 reminder template.
pub const DUNNING_DAY7_TEMPLATE: &str = "subscription-dunning-day7";
/// The audit reason for the day-14 cancel (also the
/// `apply_transition` reason on its status_changed row).
pub const DUNNING_DAY14_REASON: &str = "dunning-day-14";

/// The dunning policy, from `Settings` (ADR-0035).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DunningConfig {
    /// `PF_BILLING_DUNNING_ENABLED` — the kill-switch.
    pub enabled: bool,
    /// `PF_BILLING_DUNNING_DAY3_DAYS` — first reminder day.
    pub day3_days: u32,
    /// `PF_BILLING_DUNNING_DAY7_DAYS` — second reminder day.
    pub day7_days: u32,
    /// `PF_BILLING_DUNNING_SUSPEND_DAYS` — cancel day.
    pub suspend_days: u32,
}

impl DunningConfig {
    /// The policy as configured (defaults: enabled, 3/7/14).
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            enabled: settings.dunning_enabled,
            day3_days: settings.dunning_day3_days,
            day7_days: settings.dunning_day7_days,
            suspend_days: settings.dunning_suspend_days,
        }
    }
}

/// What [`run_dunning`] did with the past_due rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DunningReport {
    /// Whether the job actually ran (`false` = disabled by
    /// config; every count below is 0 then).
    pub ran: bool,
    /// Past_due rows scanned.
    pub scanned: usize,
    /// Day-3 reminders mailed.
    pub reminded_day3: usize,
    /// Day-7 reminders mailed.
    pub reminded_day7: usize,
    /// Subscriptions canceled at the suspend day.
    pub suspended: usize,
    /// Rows that needed nothing (not due yet, already
    /// reminded for this step, or won by a concurrent run).
    pub skipped: usize,
    /// Rows that errored (logged, counted, sweep continued).
    pub errors: usize,
}

/// What one past_due row needs at `now`. Pure — the
/// day/step matrix is unit-tested below.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DunningAction {
    /// Not due for anything yet, or already reminded for
    /// the step its day calls for.
    Skip,
    /// Mail the reminder for `day` (3 or 7).
    Remind {
        /// The reminder day (3 or 7) — also the new step.
        day: u32,
        /// The template for that day.
        template: &'static str,
    },
    /// Cancel the subscription (the suspend day reached).
    Suspend,
}

/// The pure day/step matrix: `days_past_due` is whole days
/// since `past_due_since` (floor, app clock). The suspend
/// wins over any unmailed reminder — a row that missed its
/// reminders (the job was down) is still canceled on its
/// day, never reminded late.
pub fn action_for(days_past_due: i64, step: i16, config: DunningConfig) -> DunningAction {
    if days_past_due >= i64::from(config.suspend_days) {
        return DunningAction::Suspend;
    }
    if days_past_due >= i64::from(config.day7_days) && step < 7 {
        return DunningAction::Remind {
            day: 7,
            template: DUNNING_DAY7_TEMPLATE,
        };
    }
    if days_past_due >= i64::from(config.day3_days) && step < 3 {
        return DunningAction::Remind {
            day: 3,
            template: DUNNING_DAY3_TEMPLATE,
        };
    }
    DunningAction::Skip
}

/// The transaction closure's error: small on purpose — it
/// must not recurse into [`AppError`].
#[derive(Debug)]
enum RemindTxError {
    Db(detag_core::repos::RepoError),
    /// A concurrent job run advanced the step first: the
    /// guarded bump moved nothing — roll back and let the
    /// caller count the row as skipped (the winner mailed).
    LostRace,
}

/// Run one dunning pass over every `past_due` subscription.
/// See the module docs for what each day does. A row that
/// errors is logged + counted and the sweep continues; only
/// a failure to LIST the rows fails the whole job.
pub async fn run_dunning(state: &AppState) -> Result<DunningReport, AppError> {
    let config = DunningConfig::from_settings(&state.settings);
    if !config.enabled {
        tracing::info!("dunning: disabled by PF_BILLING_DUNNING_ENABLED — no-op");
        return Ok(DunningReport {
            ran: false,
            ..DunningReport::default()
        });
    }
    let rows = subscriptions::past_due(&state.pool)
        .await
        .map_err(|_| AppError::Internal)?;
    let now = state.clock.now();
    let mut report = DunningReport {
        ran: true,
        scanned: rows.len(),
        ..DunningReport::default()
    };
    for row in rows {
        let Some(since) = row.past_due_since else {
            // Unreachable by the DB CHECK (the repo filters
            // NULLs too) — a row with no entry time has no
            // day to count from: skip loudly, never guess.
            tracing::error!(
                subscription_id = %row.id,
                "dunning: past_due row with no past_due_since — skipping"
            );
            report.errors += 1;
            continue;
        };
        let days = (now - since).num_days();
        match action_for(days, row.dunning_step, config) {
            DunningAction::Skip => report.skipped += 1,
            DunningAction::Remind { day, template } => {
                match remind(state, &row, day, template).await {
                    Ok(true) => {
                        if day == 3 {
                            report.reminded_day3 += 1;
                        } else {
                            report.reminded_day7 += 1;
                        }
                    }
                    Ok(false) => report.skipped += 1,
                    Err(err) => {
                        tracing::error!(
                            subscription_id = %row.id,
                            error = ?err,
                            "dunning: reminder failed"
                        );
                        report.errors += 1;
                    }
                }
            }
            DunningAction::Suspend => match suspend(state, &row).await {
                Ok(true) => report.suspended += 1,
                Ok(false) => report.skipped += 1,
                Err(err) => {
                    tracing::error!(
                        subscription_id = %row.id,
                        error = ?err,
                        "dunning: suspend failed"
                    );
                    report.errors += 1;
                }
            },
        }
    }
    tracing::info!(
        scanned = report.scanned,
        reminded_day3 = report.reminded_day3,
        reminded_day7 = report.reminded_day7,
        suspended = report.suspended,
        skipped = report.skipped,
        errors = report.errors,
        "dunning: pass complete"
    );
    Ok(report)
}

/// Mail one reminder: the outbox row, the audit row, and
/// the guarded step bump in one transaction. Returns `true`
/// when the mail landed, `false` when a concurrent run won
/// the step first (the caller counts it as skipped).
async fn remind(
    state: &AppState,
    row: &Subscription,
    day: u32,
    template: &str,
) -> Result<bool, AppError> {
    let user = users::by_id(&state.pool, row.user_id)
        .await
        .map_err(|_| AppError::Internal)?
        .ok_or(AppError::Internal)?;
    let plan = subscriptions::plan_for_subscription(&state.pool, row.plan_id)
        .await
        .map_err(|_| AppError::Internal)?
        .ok_or(AppError::Internal)?;
    let timeout = PoolConfig::default().transaction_timeout;
    let from_step = row.dunning_step;
    let to_step = day as i16;
    let subscription_id = row.id;
    let user_id = user.id;
    let user_email = user.email.clone();
    let plan_code = plan.code.clone();
    let template_owned = template.to_string();
    let reason = format!("dunning-day-{day}");
    let landed = tx::with_tx(&state.pool, timeout, |conn| {
        let user_email = user_email.clone();
        let plan_code = plan_code.clone();
        let template_owned = template_owned.clone();
        let reason = reason.clone();
        Box::pin(async move {
            let bumped = subscriptions::set_dunning_step_guarded(
                &mut *conn,
                subscription_id,
                from_step,
                to_step,
            )
            .await
            .map_err(RemindTxError::Db)?;
            if !bumped {
                return Err(RemindTxError::LostRace);
            }
            email_outbox::enqueue(
                &mut *conn,
                &NewOutboxMail {
                    to_hash: sha256_hex(user_email.as_bytes()),
                    template: template_owned,
                    payload: serde_json::json!({
                        "user_id": user_id,
                        "plan_code": plan_code,
                    }),
                    scheduled_at: None,
                },
            )
            .await
            .map_err(RemindTxError::Db)?;
            record(
                &mut *conn,
                &NewAuditLog {
                    actor_user_id: None,
                    actor_client: Some("dunning".to_string()),
                    action: DUNNING_REMINDER_ACTION.to_string(),
                    target_type: "subscription".to_string(),
                    target_id: Some(subscription_id.to_string()),
                    before: Some(serde_json::json!({ "dunning_step": from_step })),
                    after: Some(serde_json::json!({
                        "dunning_step": to_step,
                        "day": day,
                    })),
                    reason: Some(reason),
                },
            )
            .await
            .map_err(RemindTxError::Db)?;
            Ok(())
        })
    })
    .await;
    match landed {
        Ok(()) => Ok(true),
        Err(TxError::Inner(RemindTxError::LostRace)) => Ok(false),
        Err(TxError::Inner(RemindTxError::Db(db_err))) => {
            tracing::error!(error = ?db_err, "dunning: reminder transaction failed");
            Err(AppError::Internal)
        }
        Err(other) => {
            tracing::error!(error = ?other, "dunning: reminder transaction failed");
            Err(AppError::Internal)
        }
    }
}

/// Cancel one past_due subscription at the suspend day,
/// through the state machine (so the canceled mail + the
/// status_changed audit land with it). Returns `true` when
/// the cancel landed, `false` when the row left `past_due`
/// between the scan and the apply (recovered, or canceled
/// already — the caller counts it as skipped).
async fn suspend(state: &AppState, row: &Subscription) -> Result<bool, AppError> {
    use detag_billing::SubscriptionStatus;
    let applied = crate::services::subscription_state::apply_transition(
        state,
        row.id,
        SubscriptionStatus::Canceled,
        DUNNING_DAY14_REASON,
    )
    .await;
    match applied {
        Ok(applied) => Ok(applied.changed),
        Err(AppError::InvalidStateTransition) => Ok(false),
        Err(AppError::NotFound) => Ok(false),
        Err(err) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> DunningConfig {
        DunningConfig {
            enabled: true,
            day3_days: 3,
            day7_days: 7,
            suspend_days: 14,
        }
    }

    #[test]
    fn action_matrix() {
        let c = config();
        // (days, step, expected)
        let cases = [
            (0, 0, DunningAction::Skip),
            (2, 0, DunningAction::Skip),
            (
                3,
                0,
                DunningAction::Remind {
                    day: 3,
                    template: DUNNING_DAY3_TEMPLATE,
                },
            ),
            (3, 3, DunningAction::Skip),
            (6, 3, DunningAction::Skip),
            (
                7,
                3,
                DunningAction::Remind {
                    day: 7,
                    template: DUNNING_DAY7_TEMPLATE,
                },
            ),
            (7, 7, DunningAction::Skip),
            (13, 7, DunningAction::Skip),
            (
                13,
                0,
                DunningAction::Remind {
                    day: 7,
                    template: DUNNING_DAY7_TEMPLATE,
                },
            ),
            // The suspend wins over any unmailed reminder.
            (14, 0, DunningAction::Suspend),
            (14, 3, DunningAction::Suspend),
            (14, 7, DunningAction::Suspend),
            (30, 7, DunningAction::Suspend),
            // A future entry time (clock skew) mails nothing.
            (-1, 0, DunningAction::Skip),
        ];
        for (days, step, expected) in cases {
            assert_eq!(
                action_for(days, step, c),
                expected,
                "days={days} step={step}"
            );
        }
    }

    #[test]
    fn custom_days_move_the_matrix() {
        let c = DunningConfig {
            enabled: true,
            day3_days: 1,
            day7_days: 2,
            suspend_days: 5,
        };
        assert_eq!(
            action_for(1, 0, c),
            DunningAction::Remind {
                day: 3,
                template: DUNNING_DAY3_TEMPLATE,
            },
            "day3 follows config, not the literal 3"
        );
        assert_eq!(
            action_for(4, 3, c),
            DunningAction::Remind {
                day: 7,
                template: DUNNING_DAY7_TEMPLATE,
            }
        );
        assert_eq!(action_for(5, 7, c), DunningAction::Suspend);
    }
}
