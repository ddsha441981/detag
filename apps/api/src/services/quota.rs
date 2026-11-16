//! Quota pre-check (P1-051, FR-P-02 / FR-P-06 / FR-P-07).
//!
//! The gate before job creation: given the upload a job-create
//! request names, decide whether the caller's plan has room —
//! daily images (image uploads), daily video-seconds (video
//! uploads, the P1-050 estimator), and a free concurrency slot
//! — and surface the remaining quota in the `X-Quota-*`
//! headers on both the allowed and the denied response.
//!
//! The check reads the DURABLE snapshot
//! ([`entitlements::quota_snapshot_for_plan`]: plan
//! entitlements + today's `usage_counters` row), never the
//! Redis mirrors — so the decision agrees with the `/v1/me`
//! snapshot by construction (the P1-050 module doc said the
//! middleware reads the Redis hash; that sentence is corrected
//! in this commit — see ASSUMPTION-017). The Redis hash stays
//! the metering hot path for `UsageMeter::record`; the atomic
//! check-and-record lands with real job creation in Phase 2,
//! when a record actually follows the check.
//!
//! Check order (a request can trip several limits; the first
//! denial wins, so the code is deterministic): quota
//! dimensions first (images, then video-seconds — the codes
//! the spec names), then the concurrency slot. A video job
//! always needs at least one video-second, even when its
//! duration is unknown (the P1-050 estimator reserves 0) — a
//! plan with a 0 video-seconds limit grants no video at all,
//! so an unknown-duration video on Free is PF-3002, not a
//! free pass.
//!
//! The check is read-only: no usage is recorded and no slot
//! is acquired. Job creation is still the 501 stub, so
//! recording now would burn quota for a job that never
//! exists. A denial writes no audit row (the P1-043 rule:
//! the trail records actions, not attempts).
//!
//! The gate lives in the handler, not an axum `from_fn`
//! layer: the check needs the parsed `upload_id` from the
//! body, and a layer would have to buffer and re-inject the
//! body to read it. The service is the gate; the handler is
//! its thin shell.

use super::entitlements::{
    effective_plan, quota_snapshot_for_plan, EntitlementError, QuotaSnapshot,
};
use crate::AppState;
use chrono::NaiveDate;
use detag_core::db::DbPool;
use detag_core::error::AppError;
use detag_core::plans::{PlanCache, PlanCatalog, PlanStore};
use detag_core::repos::{jobs, uploads, RepoError};
use detag_core::usage::estimate_video_seconds;
use uuid::Uuid;

/// `X-Quota-Images-Remaining`: today's image quota left
/// (`limit - used`, floored at 0), on every job-create
/// response the pre-check runs on (allowed 501 and denied
/// 429 alike).
pub const HEADER_IMAGES_REMAINING: &str = "x-quota-images-remaining";
/// `X-Quota-Video-Seconds-Remaining`: today's video-seconds
/// quota left, same rule as [`HEADER_IMAGES_REMAINING`].
pub const HEADER_VIDEO_SECONDS_REMAINING: &str = "x-quota-video-seconds-remaining";

/// The plan-matrix link (P1-054, FR-P-06): the console's
/// plans page at `{PF_PUBLIC_BASE_URL}/plans`. The console
/// serves it in Phase 4; until then the URL is the
/// contract — the setting is the source, so the link never
/// hardcodes a host and the owner can change it without a
/// code change (ASSUMPTION-020). A trailing slash on the
/// base is trimmed, so the link never doubles it.
pub fn plans_url(public_base_url: &str) -> String {
    format!("{}/plans", public_base_url.trim_end_matches('/'))
}

/// A passed pre-check: the snapshot the headers are stamped
/// from, plus the concurrency numbers (for the log line, not
/// a header — the api-spec names only the two quota
/// headers).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuotaDecision {
    pub snapshot: QuotaSnapshot,
    pub running_jobs: i64,
    pub concurrency_limit: i64,
}

/// The pre-check outcome: allowed (with the numbers for the
/// headers) or denied (with the PF code + the snapshot for
/// the headers on the 429).
#[derive(Debug, Clone)]
pub enum QuotaOutcome {
    Allowed(QuotaDecision),
    Denied(AppError, QuotaSnapshot),
}

/// What a pre-check can fail with, besides a denial.
/// `Db` / `MissingFreePlan` are real infrastructure
/// failures (500); `UploadNotFound` is the named upload
/// missing — or belonging to another user (the P1-045 IDOR
/// rule: a foreign resource reads exactly like a missing
/// one, 404 `PF-2010`).
#[derive(Debug, thiserror::Error)]
pub enum QuotaError {
    #[error("quota check failed")]
    Db(#[from] RepoError),
    #[error("the free plan is missing from the database")]
    MissingFreePlan,
    #[error("the upload does not exist")]
    UploadNotFound,
}

impl From<EntitlementError> for QuotaError {
    fn from(err: EntitlementError) -> Self {
        match err {
            EntitlementError::Db(repo) => QuotaError::Db(repo),
            EntitlementError::MissingFreePlan => QuotaError::MissingFreePlan,
        }
    }
}

/// The quota pre-check for one job-create request: load the
/// upload (404 when missing or foreign), resolve the
/// effective plan, read the snapshot for `day`, and decide.
/// `day` is the UTC day from the app's injected clock
/// (`state.clock`) — never `Utc::now()` here, so a frozen
/// clock moves the quota day too (P1-057, ADR-0027).
///
/// - Image upload: one image is needed; `remaining < 1` is
///   `PF-3001`.
/// - Video upload: `estimate_video_seconds(duration)` is
///   needed, at least 1 (an unknown duration still costs a
///   real video's seconds — see the module docs); more than
///   `remaining` is `PF-3002`.
/// - Concurrency: `running >= plan.concurrency` is `PF-3008`
///   (ADR-0026 — PF-3005 is the system-wide queue signal,
///   not a per-user plan cap).
///
/// Quota dimensions are checked before concurrency, so a
/// request that trips both answers with the quota code.
pub async fn precheck<S: PlanStore, C: PlanCache>(
    catalog: &PlanCatalog<S, C>,
    pool: &DbPool,
    user_id: Uuid,
    upload_id: Uuid,
    day: NaiveDate,
) -> Result<QuotaOutcome, QuotaError> {
    let upload = uploads::by_id(pool, upload_id)
        .await?
        .filter(|upload| upload.user_id == user_id)
        .ok_or(QuotaError::UploadNotFound)?;
    let resolved = effective_plan(catalog, pool, user_id).await?;
    let snapshot = quota_snapshot_for_plan(pool, user_id, &resolved.plan, day).await?;
    if upload.kind == "image" && snapshot.daily_images.remaining < 1 {
        return Ok(QuotaOutcome::Denied(
            AppError::QuotaImagesExceeded,
            snapshot,
        ));
    }
    if upload.kind == "video" {
        let needed = estimate_video_seconds(upload.duration_s).max(1);
        if needed > snapshot.daily_video_seconds.remaining {
            return Ok(QuotaOutcome::Denied(
                AppError::QuotaVideoSecondsExceeded,
                snapshot,
            ));
        }
    }
    let running = jobs::count_running_for_user(pool, user_id).await?;
    let concurrency_limit = resolved.entitlements.concurrency;
    if running >= concurrency_limit {
        return Ok(QuotaOutcome::Denied(
            AppError::QuotaConcurrencyExceeded,
            snapshot,
        ));
    }
    Ok(QuotaOutcome::Allowed(QuotaDecision {
        snapshot,
        running_jobs: running,
        concurrency_limit,
    }))
}

/// The pre-check over the app state (the handler's call).
/// The quota day is the app clock's UTC day.
pub async fn precheck_for_create(
    state: &AppState,
    user_id: Uuid,
    upload_id: Uuid,
) -> Result<QuotaOutcome, QuotaError> {
    precheck(
        &state.plans,
        &state.pool,
        user_id,
        upload_id,
        state.clock.now().date_naive(),
    )
    .await
}

/// The caller's quota snapshot (P1-052).
///
/// The effective plan + today's durable snapshot — the same
/// two sources the pre-check reads, so headers stamped from
/// it agree with the gate and with `/v1/me` by construction.
/// Used by the create routes that carry headers without
/// gating (the no-body / no-`upload_id` create path, retry).
pub async fn snapshot_for_user(
    state: &AppState,
    user_id: Uuid,
) -> Result<QuotaSnapshot, QuotaError> {
    let resolved = effective_plan(&state.plans, &state.pool, user_id).await?;
    let snapshot = quota_snapshot_for_plan(
        &state.pool,
        user_id,
        &resolved.plan,
        state.clock.now().date_naive(),
    )
    .await?;
    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plans_url_appends_plans_to_the_public_base() {
        assert_eq!(
            plans_url("http://localhost:8080"),
            "http://localhost:8080/plans"
        );
        assert_eq!(
            plans_url("https://console.example.com/"),
            "https://console.example.com/plans"
        );
        assert_eq!(plans_url("https://x.example///"), "https://x.example/plans");
    }
}
