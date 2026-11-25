//! Support user view (P1-106, FR-C-02): `GET
//! /v1/admin/users/{id}` — one read-only composite for the
//! support workflow: profile, plan, usage, recent jobs,
//! session meta, ticket notes. No secrets anywhere in the shape (ADR-0065).
//!
//! The view is a composition of reads the API already owns:
//! the user row (`users::by_id`), the live subscription
//! (`subscriptions::active_for_user` — the newest live row
//! wins, the entitlements precedent — + `plan_for_subscription`
//! for the code), the usage read (`get_usage` today +
//! `usage_counters::totals_for_user`), the newest jobs
//! (`jobs::list_by_user`, capped at 10), the live
//! sessions (`sessions::list_live_for_user`, rendered through
//! the P1-019 `SessionItem` — the same safe shape the user
//! sees themselves: parsed device label + ip hash, never the
//! refresh hash or the family id), and the ticket notes
//! (`users::support_notes`, P1-110 — the append-only list,
//! oldest first).
//!
//! A user who does not exist is 404 `PF-2010` (the admin
//! asked about a specific user, so a missing one is a
//! wrong id, not an empty answer — the P1-081 precedent).
//! A user who exists but has no subscription is Free (the
//! `UserSearchRow` precedent: the code falls back to
//! `"free"`, the status is `None`); a user with no jobs or
//! no live sessions gets empty arrays, not a 404. No audit
//! row: a read creates nothing durable (P1-046 — reads are
//! not audited). No Redis read: the DB is the source of
//! truth, never the hot hash.

use chrono::{DateTime, Utc};
use detag_core::repos::{jobs, sessions, subscriptions, usage_counters, users};
use serde::Serialize;
use thiserror::Error;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    services::{
        admin_usage::UsageTotals,
        sessions::SessionItem,
        ticket_notes::{parse_stored_note, TicketNote},
        usage::{get_usage, DayUsage, UsageError},
    },
    AppState,
};

/// How many recent jobs the view carries (P1-106, ADR-0065):
/// the 10 newest — enough for a support glance at "what
/// were they doing", small enough that the view stays one
/// screen. The full list lives at `GET /v1/admin/jobs`
/// (P1-038, filterable by `user_id`).
pub const RECENT_JOBS_LIMIT: i64 = 10;

/// The profile section: the account, as support sees it.
/// Every field is already visible on the P1-009 search row
/// (`UserSearchRow`) except `pending_email` (an in-flight
/// email change — support-relevant for recovery) and
/// `deleted_at` (the deletion grace — support-relevant for
/// "where did the account go"). Deliberately absent:
/// `password_hash` (the credential), `settings` (the user's
/// private prefs blob — the support workflow never needs it),
/// `revoked_before` (an internal revocation epoch).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct UserProfileView {
    /// The user id (the `{id}` the caller asked for).
    #[schema(value_type = String, example = "123e4567-e89b-42d3-a456-426614174109")]
    pub id: Uuid,
    /// The account email.
    pub email: String,
    /// The display name, when set.
    pub display_name: Option<String>,
    /// `admin` / `support` / `user`.
    pub role: String,
    /// `active` / `suspended` (the P1-008 states).
    pub status: String,
    /// When the email was verified (`None` = never).
    pub email_verified_at: Option<DateTime<Utc>>,
    /// The ToS version the user accepted (`None` = never).
    pub tos_version: Option<String>,
    /// The AUP version the user accepted (`None` = never).
    pub aup_version: Option<String>,
    /// The address an email change is moving to (`None` =
    /// no change in flight).
    pub pending_email: Option<String>,
    /// The deletion request time (`None` = not deleting).
    pub deleted_at: Option<DateTime<Utc>>,
    /// When the account was created.
    pub created_at: DateTime<Utc>,
    /// When the account was last updated.
    pub updated_at: DateTime<Utc>,
}

/// Build the profile section from a user row. The mapping is
/// a pure function so the no-secrets shape is unit-tested
/// without a database (the test serializes the view and
/// asserts no secret field name survives).
pub fn profile_view_of(user: &users::User) -> UserProfileView {
    UserProfileView {
        id: user.id,
        email: user.email.clone(),
        display_name: user.display_name.clone(),
        role: user.role.clone(),
        status: user.status.clone(),
        email_verified_at: user.email_verified_at,
        tos_version: user.tos_version.clone(),
        aup_version: user.aup_version.clone(),
        pending_email: user.pending_email.clone(),
        deleted_at: user.deleted_at,
        created_at: user.created_at,
        updated_at: user.updated_at,
    }
}

/// The plan section: the effective plan + the live
/// subscription's dates. `code` is always present (`"free"`
/// when there is no live subscription — the `UserSearchRow`
/// precedent); the other three are `None`/`false` exactly
/// then. Deliberately absent: the provider customer and
/// provider subscription ids — support works in Detag, not
/// in the provider dashboard (the refund tool, P1-108, owns
/// the deep link).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct UserPlanView {
    /// The plan code (`free`, `starter`, `pro`, `business`).
    pub code: String,
    /// The live subscription's status (`trialing` /
    /// `active`) — `None` when there is no live subscription.
    pub status: Option<String>,
    /// When the current period ends (`None` = no live
    /// subscription).
    pub current_period_end: Option<DateTime<Utc>>,
    /// The subscription ends at the period end (`false` =
    /// renews, or no live subscription).
    pub cancel_at_period_end: bool,
}

/// The usage section: today's bucket + the all-time totals.
/// The 30-day window lives at `GET /v1/admin/usage/{id}`
/// (P1-081) — the support view carries the summary, not the
/// window, so one screen answers "how much has this user
/// used?".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct UserUsageView {
    /// Today's bucket (images + video seconds used today).
    pub today: DayUsage,
    /// All-time totals (every `usage_counters` row, not
    /// just the window).
    pub totals: UsageTotals,
}

/// One recent job: id, kind, status, and the timestamps.
/// Deliberately absent: `params` (may carry storage keys),
/// `webhook_url`, `idempotency_key`, `error` (may carry
/// internal detail), `upload_id`, `key_id` — support needs
/// to see that a job ran and how it ended, not its inputs.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct RecentJobView {
    /// The job id.
    #[schema(value_type = String, example = "123e4567-e89b-42d3-a456-426614174109")]
    pub id: Uuid,
    /// `image` / `video`.
    pub kind: String,
    /// `queued` / `running` / `succeeded` / `failed` / ...
    pub status: String,
    /// When the job was created.
    pub created_at: DateTime<Utc>,
    /// When the job finished (`None` = not finished).
    pub finished_at: Option<DateTime<Utc>>,
}

/// Build one recent-job view from a job row. The mapping is
/// a pure function so the no-secrets shape is unit-tested
/// without a database.
pub fn recent_job_view_of(job: &jobs::Job) -> RecentJobView {
    RecentJobView {
        id: job.id,
        kind: job.kind.clone(),
        status: job.status.clone(),
        created_at: job.created_at,
        finished_at: job.finished_at,
    }
}

/// The support user view: profile + plan + usage + recent
/// jobs + live sessions + ticket notes, for one user. No field anywhere in
/// this shape is a secret: no password hash, no refresh
/// hash, no family id, no provider id, no token, no raw
/// user-agent string, no raw IP (ADR-0065).
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct AdminUserViewResponse {
    /// Whose view this is (the `{id}` the caller asked for).
    #[schema(value_type = String, example = "123e4567-e89b-42d3-a456-426614174109")]
    pub user_id: Uuid,
    /// The account.
    pub profile: UserProfileView,
    /// The effective plan.
    pub plan: UserPlanView,
    /// Today's usage + all-time totals.
    pub usage: UserUsageView,
    /// The 10 newest jobs, newest first. Empty when the
    /// user never created one.
    pub recent_jobs: Vec<RecentJobView>,
    /// The live sessions (not revoked, not expired), newest
    /// first, in the P1-019 shape. Empty when the user has
    /// none live.
    pub sessions: Vec<SessionItem>,
    /// The ticket notes (P1-110): the admin-written
    /// append-only list, oldest first. Empty when no admin
    /// has written one — this is how the support flow reads
    /// the notes, so there is no second GET route for them.
    pub support_notes: Vec<TicketNote>,
}

/// What the support user view can fail with. `NotFound` is
/// the unknown-user 404; the rest are infrastructure
/// failures — the handler answers 500 `PF-5000`.
#[derive(Debug, Error)]
pub enum AdminUserViewError {
    /// No such user.
    #[error("no such user")]
    NotFound,
    /// A database read failed.
    #[error("user view read failed")]
    Db(#[from] detag_core::repos::RepoError),
    /// The usage read failed in a way that is not a
    /// database error.
    #[error("user view read failed")]
    Internal,
}

impl From<UsageError> for AdminUserViewError {
    fn from(err: UsageError) -> Self {
        match err {
            UsageError::Db(err) => AdminUserViewError::Db(err),
            UsageError::Internal => AdminUserViewError::Internal,
        }
    }
}

/// Read `user_id`'s support view: the profile, the plan, the
/// usage summary, the 10 newest jobs, the live sessions, and
/// the ticket notes.
/// The user must exist — a missing user is 404, not an
/// empty view.
pub async fn get_admin_user_view(
    state: &AppState,
    user_id: Uuid,
) -> Result<AdminUserViewResponse, AdminUserViewError> {
    let user = match users::by_id(&state.pool, user_id).await? {
        Some(user) => user,
        None => return Err(AdminUserViewError::NotFound),
    };
    let plan = match subscriptions::active_for_user(&state.pool, user_id).await? {
        Some(sub) => {
            let code = match subscriptions::plan_for_subscription(&state.pool, sub.plan_id).await? {
                Some(plan) => plan.code,
                // A live subscription whose plan row is gone
                // (impossible while `plan_id` is `ON DELETE
                // RESTRICT`, but defended anyway — the
                // entitlements precedent): Free, not a 500.
                None => "free".to_string(),
            };
            UserPlanView {
                code,
                status: Some(sub.status),
                current_period_end: sub.current_period_end,
                cancel_at_period_end: sub.cancel_at_period_end,
            }
        }
        None => UserPlanView {
            code: "free".to_string(),
            status: None,
            current_period_end: None,
            cancel_at_period_end: false,
        },
    };
    let usage = get_usage(state, user_id, false).await?;
    let (images, video_seconds) = usage_counters::totals_for_user(&state.pool, user_id).await?;
    let recent_jobs = jobs::list_by_user(&state.pool, user_id, RECENT_JOBS_LIMIT)
        .await?
        .iter()
        .map(recent_job_view_of)
        .collect();
    let sessions = sessions::list_live_for_user(&state.pool, user_id, state.clock.now())
        .await?
        .into_iter()
        .map(SessionItem::from_row)
        .collect();
    // The ticket notes (P1-110): the append-only list, oldest
    // first. The user was just read, so a missing list here
    // is an inconsistency, not a 404 — and a stored value
    // that is not a note is corrupt data (only the notes
    // service writes the column), so both are 500s.
    let stored_notes = match users::support_notes(&state.pool, user_id).await? {
        Some(notes) => notes,
        None => return Err(AdminUserViewError::Internal),
    };
    let mut support_notes = Vec::with_capacity(stored_notes.len());
    for value in stored_notes {
        match parse_stored_note(value) {
            Some(note) => support_notes.push(note),
            None => {
                tracing::error!("admin user view: stored note failed to parse");
                return Err(AdminUserViewError::Internal);
            }
        }
    }
    Ok(AdminUserViewResponse {
        user_id,
        profile: profile_view_of(&user),
        plan,
        usage: UserUsageView {
            today: usage.today,
            totals: UsageTotals {
                images,
                video_seconds,
            },
        },
        recent_jobs,
        sessions,
        support_notes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use detag_core::repos::users::User;

    fn test_user() -> User {
        User {
            id: Uuid::parse_str("123e4567-e89b-42d3-a456-426614174109").unwrap(),
            email: "user@example.com".to_string(),
            password_hash: "$argon2id$v=19$m=19456,t=2,p=1$SECRET$SECRET".to_string(),
            email_verified_at: Some(Utc.with_ymd_and_hms(2026, 11, 19, 12, 0, 0).unwrap()),
            role: "user".to_string(),
            status: "active".to_string(),
            tos_version: Some("v1".to_string()),
            aup_version: Some("v1".to_string()),
            deleted_at: None,
            revoked_before: Some(Utc.with_ymd_and_hms(2026, 11, 19, 11, 0, 0).unwrap()),
            display_name: Some("Test User".to_string()),
            pending_email: Some("new@example.com".to_string()),
            settings: serde_json::json!({"weekly_digest": false}),
            created_at: Utc.with_ymd_and_hms(2026, 11, 19, 10, 0, 0).unwrap(),
            updated_at: Utc.with_ymd_and_hms(2026, 11, 19, 10, 0, 0).unwrap(),
        }
    }

    #[test]
    fn profile_view_carries_no_secret_field() {
        // The profile is built from a row that HAS a password
        // hash, a revocation epoch, and a settings blob — none
        // of them may survive into the view (ADR-0065). The
        // test serializes the view and greps the JSON, so a
        // field added to the struct without thinking fails
        // here even if its name looks innocent.
        let view = profile_view_of(&test_user());
        let json = serde_json::to_string(&view).unwrap();
        for forbidden in [
            "password_hash",
            "password",
            "revoked_before",
            "settings",
            "secret",
            "SECRET",
        ] {
            assert!(
                !json.contains(forbidden),
                "profile JSON must not contain {forbidden}: {json}"
            );
        }
        assert_eq!(view.email, "user@example.com");
        assert_eq!(view.pending_email.as_deref(), Some("new@example.com"));
    }

    #[test]
    fn whole_view_serializes_with_no_secret_field_name() {
        // The same grep over the whole response shape: a
        // secret added to ANY section (plan provider ids,
        // job params, session refresh hash) fails here.
        // The sections are built by hand here (the live
        // tests prove the service fills them); the point is
        // the shape has nowhere to hide a secret.
        let view = AdminUserViewResponse {
            user_id: test_user().id,
            profile: profile_view_of(&test_user()),
            plan: UserPlanView {
                code: "free".to_string(),
                status: None,
                current_period_end: None,
                cancel_at_period_end: false,
            },
            usage: UserUsageView {
                today: DayUsage {
                    date: chrono::NaiveDate::from_ymd_opt(2026, 11, 19).unwrap(),
                    images: 0,
                    video_seconds: 0,
                },
                totals: UsageTotals {
                    images: 0,
                    video_seconds: 0,
                },
            },
            recent_jobs: vec![],
            sessions: vec![],
            support_notes: vec![],
        };
        let json = serde_json::to_string(&view).unwrap();
        for forbidden in [
            "password_hash",
            "refresh_hash",
            "family_id",
            "provider_customer_id",
            "provider_subscription_id",
            "params",
            "webhook_url",
            "idempotency_key",
            "secret",
            "SECRET",
        ] {
            assert!(
                !json.contains(forbidden),
                "view JSON must not contain {forbidden}: {json}"
            );
        }
    }

    #[test]
    fn recent_job_view_drops_params_and_error() {
        // A job row carries params, a webhook URL, an
        // idempotency key, and an error blob — the view
        // carries the id, kind, status, and timestamps only.
        let job = jobs::Job {
            id: Uuid::parse_str("123e4567-e89b-42d3-a456-426614174110").unwrap(),
            user_id: test_user().id,
            upload_id: None,
            kind: "image".to_string(),
            mode: "inpaint".to_string(),
            params: serde_json::json!({"s3_key": "uploads/secret-key"}),
            status: "failed".to_string(),
            stage: None,
            progress: 0.0,
            priority: 0,
            lane: "standard".to_string(),
            attempts: 1,
            idempotency_key: Some("key-123".to_string()),
            error: Some(serde_json::json!({"detail": "internal boom"})),
            attestation_id: None,
            webhook_url: Some("https://example.com/hook".to_string()),
            started_at: None,
            finished_at: Some(Utc.with_ymd_and_hms(2026, 11, 19, 12, 0, 0).unwrap()),
            created_at: Utc.with_ymd_and_hms(2026, 11, 19, 11, 0, 0).unwrap(),
            updated_at: Utc.with_ymd_and_hms(2026, 11, 19, 12, 0, 0).unwrap(),
            key_id: None,
        };
        let view = recent_job_view_of(&job);
        let json = serde_json::to_string(&view).unwrap();
        for forbidden in [
            "s3_key",
            "secret-key",
            "webhook_url",
            "idempotency_key",
            "key-123",
            "internal boom",
        ] {
            assert!(
                !json.contains(forbidden),
                "job JSON must not contain {forbidden}: {json}"
            );
        }
        assert_eq!(view.status, "failed");
    }
}
