//! Onboarding checklist service (P1-096): GET /v1/me/onboarding.
//!
//! The checklist is a READ MODEL over rows that already
//! exist — no new table, no new event, no write on the read
//! (a GET writes nothing: the P1-046 / P1-087 rule that reads
//! never audit). The five steps, in the order the web shows
//! them:
//!
//! - `verify_email` — `users.email_verified_at` (set by the
//!   P1-002 verify flow).
//! - `accept_tos` — the P1-095 consent pair is up to date
//!   (`ConsentStatus::from_user(...).up_to_date`). Its
//!   completion time is the latest `user.consent_accepted`
//!   audit row for the user, falling back to
//!   `users.created_at` when up to date with no audit row:
//!   register accepts the current versions at creation and
//!   writes no audit row (the consent-test precedent), so
//!   creation IS the acceptance for a never-re-accepted
//!   account. A config bump reopens the step (`completed_at`
//!   goes back to null) until the re-accept lands.
//! - `first_upload` / `first_job` — `MIN(created_at)` of the
//!   user's rows (no status filter: an aborted upload or a
//!   held job still proves the user did the action once).
//! - `create_api_key` (optional) — `MIN(created_at)` of the
//!   user's keys, live or not: a revoked key still proves the
//!   action was done once (ASSUMPTION-057).
//!
//! `all_done` means the four REQUIRED steps are complete —
//! the optional key never blocks it. `completed_count` /
//! `total_count` are raw counts over all five steps, so a
//! fully-onboarded user without a key reads 4/5 with
//! `all_done: true`.
//!
//! The steps' `completed_at` values ARE the "completion
//! events" the ticket spec names: the web consumes them to
//! show when each step completed. No new event is emitted and
//! no completion is recorded anywhere else — P1-100 owns the
//! web flow contract (screens, copy, drop-off metric events).

use super::consent::ConsentStatus;
use crate::AppState;
use chrono::{DateTime, Utc};
use detag_core::repos::{api_keys, audit_logs, jobs, uploads, users::User};
use serde::Serialize;
use thiserror::Error;
use utoipa::ToSchema;

/// One checklist step: a stable id the web keys on, a human
/// title, whether the step is optional, and its completion
/// state. `completed_at` is null until the step completes.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct OnboardingStep {
    /// Stable step id (`verify_email`, `accept_tos`,
    /// `first_upload`, `first_job`, `create_api_key`).
    pub id: String,
    /// Human title for the web checklist.
    pub title: String,
    /// True only for `create_api_key` — the optional step
    /// never blocks `all_done`.
    pub optional: bool,
    /// Whether the step is complete right now.
    pub completed: bool,
    /// When the step completed (`None` until then).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
}

/// The checklist response: the five steps in display order,
/// raw completion counts, and whether the required steps are
/// all complete.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct OnboardingChecklist {
    /// The five steps, in display order.
    pub steps: Vec<OnboardingStep>,
    /// Completed steps (all five counted, optional included).
    pub completed_count: usize,
    /// Total steps (5).
    pub total_count: usize,
    /// True when the four required steps are complete — the
    /// optional API-key step never blocks this.
    pub all_done: bool,
}

/// What a checklist read can fail with (a DB read only).
#[derive(Debug, Error)]
pub enum OnboardingError {
    #[error("onboarding read failed")]
    Internal,
}

/// The inputs [`build_checklist`] needs, gathered by
/// [`get_onboarding`] — kept separate so the step logic is a
/// pure function the unit tests can pin without a database.
struct StepInputs {
    email_verified_at: Option<DateTime<Utc>>,
    consent_up_to_date: bool,
    /// Latest `user.consent_accepted` audit row for the user
    /// (`None` when the user never re-accepted).
    consent_accepted_at: Option<DateTime<Utc>>,
    user_created_at: DateTime<Utc>,
    first_upload_at: Option<DateTime<Utc>>,
    first_job_at: Option<DateTime<Utc>>,
    first_api_key_at: Option<DateTime<Utc>>,
}

/// Build the checklist from the gathered inputs. Pure: the
/// step transitions (and the bump reopening `accept_tos`) are
/// pinned by unit tests here, not by the live suite.
fn build_checklist(inputs: StepInputs) -> OnboardingChecklist {
    // accept_tos: complete only while up to date. Its time is
    // the latest re-accept, else the account's creation (the
    // register-time acceptance) — never an old re-accept after
    // a bump reopened the step.
    let accept_tos_at = if inputs.consent_up_to_date {
        inputs.consent_accepted_at.or(Some(inputs.user_created_at))
    } else {
        None
    };
    let steps = vec![
        OnboardingStep {
            id: "verify_email".to_string(),
            title: "Verify your email".to_string(),
            optional: false,
            completed: inputs.email_verified_at.is_some(),
            completed_at: inputs.email_verified_at,
        },
        OnboardingStep {
            id: "accept_tos".to_string(),
            title: "Accept the Terms of Service".to_string(),
            optional: false,
            completed: inputs.consent_up_to_date,
            completed_at: accept_tos_at,
        },
        OnboardingStep {
            id: "first_upload".to_string(),
            title: "Upload your first image".to_string(),
            optional: false,
            completed: inputs.first_upload_at.is_some(),
            completed_at: inputs.first_upload_at,
        },
        OnboardingStep {
            id: "first_job".to_string(),
            title: "Create your first job".to_string(),
            optional: false,
            completed: inputs.first_job_at.is_some(),
            completed_at: inputs.first_job_at,
        },
        OnboardingStep {
            id: "create_api_key".to_string(),
            title: "Create an API key".to_string(),
            optional: true,
            completed: inputs.first_api_key_at.is_some(),
            completed_at: inputs.first_api_key_at,
        },
    ];
    let completed_count = steps.iter().filter(|step| step.completed).count();
    let total_count = steps.len();
    let all_done = steps
        .iter()
        .filter(|step| !step.optional)
        .all(|step| step.completed);
    OnboardingChecklist {
        steps,
        completed_count,
        total_count,
        all_done,
    }
}

/// GET /v1/me/onboarding: the checklist for the caller.
/// Reads only — five small reads (the user row is already in
/// hand; the audit read is one user's rows via the target
/// index; the three `MIN(created_at)` reads are per-user).
/// A DB failure is 500; there is no other failure mode.
pub async fn get_onboarding(
    state: &AppState,
    user: &User,
) -> Result<OnboardingChecklist, OnboardingError> {
    let settings = &state.settings;
    let consent_up_to_date = ConsentStatus::from_user(user, settings).up_to_date;
    // The latest re-accept: the user's audit rows, filtered to
    // the consent action. A read, not a write — and the list
    // is bounded to one user's rows.
    let consent_accepted_at =
        audit_logs::list_for_target(&state.pool, "user", &user.id.to_string())
            .await
            .map_err(|err| {
                tracing::error!(error = ?err, "onboarding: consent audit read failed");
                OnboardingError::Internal
            })?
            .iter()
            .filter(|row| row.action == "user.consent_accepted")
            .map(|row| row.created_at)
            .max();
    let first_upload_at = uploads::first_created_at_for_user(&state.pool, user.id)
        .await
        .map_err(|err| {
            tracing::error!(error = ?err, "onboarding: first upload read failed");
            OnboardingError::Internal
        })?;
    let first_job_at = jobs::first_created_at_for_user(&state.pool, user.id)
        .await
        .map_err(|err| {
            tracing::error!(error = ?err, "onboarding: first job read failed");
            OnboardingError::Internal
        })?;
    let first_api_key_at = api_keys::first_created_at_for_user(&state.pool, user.id)
        .await
        .map_err(|err| {
            tracing::error!(error = ?err, "onboarding: first api key read failed");
            OnboardingError::Internal
        })?;
    Ok(build_checklist(StepInputs {
        email_verified_at: user.email_verified_at,
        consent_up_to_date,
        consent_accepted_at,
        user_created_at: user.created_at,
        first_upload_at,
        first_job_at,
        first_api_key_at,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step<'a>(checklist: &'a OnboardingChecklist, id: &str) -> &'a OnboardingStep {
        checklist
            .steps
            .iter()
            .find(|step| step.id == id)
            .unwrap_or_else(|| panic!("step {id} missing"))
    }

    fn inputs(
        email_verified_at: Option<DateTime<Utc>>,
        consent_up_to_date: bool,
        consent_accepted_at: Option<DateTime<Utc>>,
        user_created_at: DateTime<Utc>,
        first_upload_at: Option<DateTime<Utc>>,
        first_job_at: Option<DateTime<Utc>>,
        first_api_key_at: Option<DateTime<Utc>>,
    ) -> StepInputs {
        StepInputs {
            email_verified_at,
            consent_up_to_date,
            consent_accepted_at,
            user_created_at,
            first_upload_at,
            first_job_at,
            first_api_key_at,
        }
    }

    #[test]
    fn fresh_user_has_only_tos_done_and_is_not_all_done() {
        let created = chrono::Utc::now();
        let checklist = build_checklist(inputs(None, true, None, created, None, None, None));
        assert_eq!(checklist.total_count, 5);
        assert_eq!(checklist.completed_count, 1);
        assert!(!checklist.all_done);
        assert!(!step(&checklist, "verify_email").completed);
        assert!(step(&checklist, "verify_email").completed_at.is_none());
        let tos = step(&checklist, "accept_tos");
        assert!(tos.completed);
        // No re-accept audit row: the acceptance time is the
        // account's creation (register accepted at creation).
        assert_eq!(tos.completed_at, Some(created));
        assert!(!step(&checklist, "first_upload").completed);
        assert!(!step(&checklist, "first_job").completed);
        let key = step(&checklist, "create_api_key");
        assert!(!key.completed);
        assert!(key.optional);
    }

    #[test]
    fn required_done_without_key_is_all_done_at_four_of_five() {
        let created = chrono::Utc::now();
        let now = chrono::Utc::now();
        let checklist = build_checklist(inputs(
            Some(now),
            true,
            None,
            created,
            Some(now),
            Some(now),
            None,
        ));
        assert_eq!(checklist.completed_count, 4);
        assert_eq!(checklist.total_count, 5);
        // The optional key never blocks all_done.
        assert!(checklist.all_done);
        assert_eq!(step(&checklist, "verify_email").completed_at, Some(now));
        assert_eq!(step(&checklist, "first_upload").completed_at, Some(now));
        assert_eq!(step(&checklist, "first_job").completed_at, Some(now));
    }

    #[test]
    fn all_five_done_is_five_of_five() {
        let created = chrono::Utc::now();
        let now = chrono::Utc::now();
        let checklist = build_checklist(inputs(
            Some(now),
            true,
            None,
            created,
            Some(now),
            Some(now),
            Some(now),
        ));
        assert_eq!(checklist.completed_count, 5);
        assert!(checklist.all_done);
        assert!(step(&checklist, "create_api_key").completed);
    }

    #[test]
    fn bump_reopens_tos_step_and_drops_its_time() {
        let created = chrono::Utc::now();
        let reaccept = chrono::Utc::now();
        // Up to date with a re-accept audit row: the row's
        // time wins over the creation fallback.
        let checklist = build_checklist(inputs(
            Some(created),
            true,
            Some(reaccept),
            created,
            None,
            None,
            None,
        ));
        assert_eq!(step(&checklist, "accept_tos").completed_at, Some(reaccept));
        // After a bump the step is incomplete and its time is
        // gone — even though the old re-accept row still exists.
        let checklist = build_checklist(inputs(
            Some(created),
            false,
            Some(reaccept),
            created,
            None,
            None,
            None,
        ));
        let tos = step(&checklist, "accept_tos");
        assert!(!tos.completed);
        assert!(tos.completed_at.is_none());
        assert!(!checklist.all_done);
    }

    #[test]
    fn steps_come_in_display_order() {
        let created = chrono::Utc::now();
        let checklist = build_checklist(inputs(None, false, None, created, None, None, None));
        let ids: Vec<&str> = checklist
            .steps
            .iter()
            .map(|step| step.id.as_str())
            .collect();
        assert_eq!(
            ids,
            [
                "verify_email",
                "accept_tos",
                "first_upload",
                "first_job",
                "create_api_key"
            ]
        );
        assert_eq!(checklist.completed_count, 0);
        assert!(!checklist.all_done);
    }
}
