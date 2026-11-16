//! Admin plan editor (P1-056, FR-P-01): `GET` / `PUT`
//! `/v1/admin/plans`.
//!
//! The GET lists the catalog (cheapest first) as the editor
//! shows it: code, price, currency, Stripe price id, and the
//! parsed entitlements — the same seven keys the profile and
//! the quota middleware read, so the console, the snapshot
//! and the editor can never disagree about what a plan
//! grants.
//!
//! The PUT is a batch edit, not a full replace: `plans`
//! carries per-plan updates (keyed by `code`; absent fields
//! are unchanged), `remove` carries the codes to delete.
//! A plan absent from both lists is untouched — an edit
//! that omits a plan must never delete it by accident. The
//! whole PUT applies in one transaction: a failed removal
//! guard rolls the updates back too.
//!
//! Validation (422 `PF-2002` with `field_errors`, via the
//! handler's `ValidatedJson`): every entitlement object is
//! the full seven keys, numerics non-negative, `lane` is
//! `standard` | `priority`, `features` holds only the known
//! names (`api_keys`, `webhooks`, `batch`) with bool values,
//! `amount` is paise `>= 0` and fits `i32`, `currency` is
//! three uppercase letters, a Stripe price id — when set —
//! is `price_` + alphanumerics. Two owner defaults are
//! enforced, not just documented: Free stays ₹0 (its amount
//! must be 0) and Free carries no Stripe price (it has no
//! price to map); both are 422s, not silent clamps
//! (ASSUMPTION-022).
//!
//! Removal guards (the service, after validation): an
//! unknown code is 404 `PF-2010`; the `free` plan cannot be
//! removed (422 — it is the resolution fallback, and
//! `effective_plan` errors without it); a plan with any
//! subscription row — live or historical — is 409
//! `PF-2012`: `plan_id` is `ON DELETE RESTRICT`, so even a
//! plan whose subscriptions are all `past_due` / `canceled`
//! cannot be deleted at the DB level, and the service
//! answers 409 instead of surfacing a foreign-key 500.
//!
//! The handler invalidates the catalog after the commit, so
//! the next read — profile, quota pre-check, feature gate —
//! sees the edit at once; the 5-minute TTL only bounds
//! staleness for edits made outside this API (direct SQL,
//! another process). The write is audited by the P1-046
//! middleware (`admin.plans.updated`, target type `plans`)
//! from the route metadata — the handler attaches the
//! changed codes as the audit `after`, and does NOT mark
//! `AuditHandled`.
//!
//! No plan is ever created here: the catalog is the three
//! seeded plans (FR-P-01); a new plan code is a seed /
//! migration change, documented by P1-058 (ASSUMPTION-022).

use crate::AppState;
use detag_core::{
    db::PoolConfig,
    error::FieldError,
    plans::Entitlements,
    repos::{plans, subscriptions, RepoError},
    tx::{self, TxError},
};
use detag_validator::{Validate, ValidationErrors};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use utoipa::ToSchema;

/// What the editor can fail with.
#[derive(Debug, thiserror::Error)]
pub enum PlanEditorError {
    #[error("plan edit validation failed")]
    Validation(ValidationErrors),
    #[error("plan not found")]
    NotFound,
    #[error("plan has subscriptions")]
    HasSubscriptions,
    #[error("plan edit failed")]
    Internal,
}

/// The transaction closure's error: small on purpose — it must
/// not recurse into [`PlanEditorError`].
#[derive(Debug)]
enum PlanTxError {
    NotFound,
    HasSubscriptions,
    Db(RepoError),
}

/// The feature names the editor accepts in the `features`
/// map (FR-P-01). `priority_lane` is NOT one: it reads the
/// `lane` field, never the map (P1-055, ASSUMPTION-021) — a
/// `priority_lane` entry in an edit is an unknown feature,
/// not a lane change.
const KNOWN_FEATURES: [&str; 3] = ["api_keys", "webhooks", "batch"];

/// One plan, as the editor shows it and returns it: the
/// stored row with the entitlements parsed (canonical — the
/// seven keys, so a stored jsonb with extra keys round-trips
/// as the effective entitlements, never as raw jsonb).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct PlanView {
    /// Plan code (`free` / `pro` / `business`).
    pub code: String,
    /// Price in paise (`amount` in the DB).
    pub amount: i32,
    /// ISO currency code (`INR`).
    pub currency: String,
    /// Stripe price id, or null when the plan has no price
    /// (Free).
    pub stripe_price_id: Option<String>,
    /// The parsed entitlements (the seven keys).
    pub entitlements: Entitlements,
}

impl From<&plans::Plan> for PlanView {
    fn from(plan: &plans::Plan) -> Self {
        PlanView {
            code: plan.code.clone(),
            amount: plan.amount,
            currency: plan.currency.clone(),
            stripe_price_id: plan.stripe_price_id.clone(),
            entitlements: Entitlements::from_value(&plan.entitlements),
        }
    }
}

/// The catalog response: all plans, cheapest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct PlansResponse {
    /// All plans, cheapest first (by `amount`).
    pub plans: Vec<PlanView>,
}

/// The seven entitlements of one plan edit. Every field is
/// required when the object is present — a partial
/// entitlements object is a 422, never a merge (the
/// fail-closed parse would read a missing key as 0, so a
/// half-written object could silently ungrant a quota).
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct PlanEntitlementsEdit {
    /// Images per UTC day.
    pub daily_images: Option<i64>,
    /// Video seconds per UTC day.
    pub daily_video_seconds: Option<i64>,
    /// Largest single upload, in bytes.
    pub max_bytes: Option<i64>,
    /// How long a finished result is kept, in hours.
    pub result_ttl_hours: Option<i64>,
    /// Concurrent running jobs for the user.
    pub concurrency: Option<i64>,
    /// `standard` or `priority`.
    pub lane: Option<String>,
    /// Named feature flags (only the known names, bool only).
    pub features: Option<HashMap<String, bool>>,
}

/// One plan update, keyed by `code`. Absent fields are
/// unchanged — only `entitlements`, when present, must be
/// the full object.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct PlanEdit {
    /// Plan code (`free` / `pro` / `business`).
    pub code: String,
    /// New price in paise (`>= 0`); Free must stay 0.
    pub amount: Option<i64>,
    /// New ISO currency code (three uppercase letters).
    pub currency: Option<String>,
    /// New Stripe price id: a string sets it, `null` clears
    /// it, absent leaves it unchanged. Free takes no price.
    /// The `deserialize_with` keeps `null` distinct from
    /// missing — plain serde derive collapses the two (the
    /// same trap `services/profile.rs` documents for its
    /// PATCH body).
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub stripe_price_id: Option<Option<String>>,
    /// New entitlements (the full seven keys).
    pub entitlements: Option<PlanEntitlementsEdit>,
}

/// Deserialize a double `Option` so JSON `null` means "clear"
/// (`Some(None)`) while a missing field means "leave unchanged"
/// (`None`, via `#[serde(default)]`). Plain serde derive
/// collapses the two — `Option<Option<T>>` deserializes `null`
/// as the outer `None` (proven by the profile PATCH body's
/// `patch_request_distinguishes_null_from_missing` unit test)
/// — so the explicit `deserialize_with` is what keeps them
/// apart. Identical in shape to `services/profile.rs`'s
/// helper; kept local so each service module stays
/// self-contained.
fn deserialize_double_option<'de, D>(deserializer: D) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Some(Option::deserialize(deserializer)?))
}

/// The PUT body: per-plan updates plus the codes to remove.
/// Both fields are optional; a body with neither is a
/// no-op 200 returning the unchanged catalog.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct UpdatePlansRequest {
    /// Plan updates, keyed by `code`.
    #[serde(default)]
    pub plans: Option<Vec<PlanEdit>>,
    /// Plan codes to delete (each guarded: no active subs).
    #[serde(default)]
    pub remove: Option<Vec<String>>,
}

/// A Stripe price id is `price_` + alphanumerics (the shape
/// Stripe issues in test and live mode alike). The check is
/// deliberately loose — it rejects typos, not future Stripe
/// id shapes.
fn is_stripe_price_id(value: &str) -> bool {
    let Some(rest) = value.strip_prefix("price_") else {
        return false;
    };
    !rest.is_empty() && rest.chars().all(|c| c.is_ascii_alphanumeric())
}

impl Validate for UpdatePlansRequest {
    fn validate(&self) -> Result<(), ValidationErrors> {
        let mut errors = ValidationErrors::default();
        let plans = self.plans.as_deref().unwrap_or(&[]);
        let remove = self.remove.as_deref().unwrap_or(&[]);

        // Duplicate codes in one list, and a code in both
        // lists, are contradictory requests, not two edits.
        let mut seen = HashSet::new();
        for (index, edit) in plans.iter().enumerate() {
            if !seen.insert(edit.code.as_str()) {
                errors.push(FieldError::new(
                    format!("plans[{index}].code"),
                    "duplicate_plan_code",
                    "each plan code may appear only once",
                ));
            }
        }
        let mut remove_seen = HashSet::new();
        for (index, code) in remove.iter().enumerate() {
            if !remove_seen.insert(code.as_str()) {
                errors.push(FieldError::new(
                    format!("remove[{index}]"),
                    "duplicate_plan_code",
                    "each plan code may appear only once",
                ));
            }
            if seen.contains(code.as_str()) {
                errors.push(FieldError::new(
                    format!("remove[{index}]"),
                    "plan_in_both_lists",
                    "a plan cannot be updated and removed in one request",
                ));
            }
        }

        for (index, edit) in plans.iter().enumerate() {
            let at = |field: &str| format!("plans[{index}].{field}");
            if edit.code.trim().is_empty() {
                errors.push(FieldError::new(at("code"), "required", "is required"));
            }
            if let Some(amount) = edit.amount {
                if amount < 0 {
                    errors.push(FieldError::new(
                        at("amount"),
                        "must_be_non_negative",
                        "must be 0 or more paise",
                    ));
                } else if amount > i64::from(i32::MAX) {
                    errors.push(FieldError::new(
                        at("amount"),
                        "too_large",
                        "exceeds the largest storable price",
                    ));
                } else if edit.code == "free" && amount != 0 {
                    // Free stays ₹0 — the owner default, enforced
                    // here so an edit cannot price the fallback
                    // plan (ASSUMPTION-022).
                    errors.push(FieldError::new(
                        at("amount"),
                        "free_plan_must_be_zero",
                        "the free plan must stay free",
                    ));
                }
            }
            if let Some(currency) = &edit.currency {
                if currency.len() != 3 || !currency.chars().all(|c| c.is_ascii_uppercase()) {
                    errors.push(FieldError::new(
                        at("currency"),
                        "invalid_currency",
                        "must be a three-letter uppercase ISO code",
                    ));
                }
            }
            if let Some(Some(price_id)) = &edit.stripe_price_id {
                if !is_stripe_price_id(price_id) {
                    errors.push(FieldError::new(
                        at("stripe_price_id"),
                        "invalid_stripe_price_id",
                        "must look like price_<alphanumerics>",
                    ));
                } else if edit.code == "free" {
                    // Free has no Stripe price to map.
                    errors.push(FieldError::new(
                        at("stripe_price_id"),
                        "free_plan_has_no_price",
                        "the free plan takes no Stripe price",
                    ));
                }
            }
            if let Some(ent) = &edit.entitlements {
                let ent_at = |field: &str| format!("plans[{index}].entitlements.{field}");
                // Each numeric must be present and non-negative.
                // A missing key reads as 0 in the fail-closed
                // parse, so accepting a partial object could
                // silently ungrant a quota.
                for (field, value) in [
                    ("daily_images", ent.daily_images),
                    ("daily_video_seconds", ent.daily_video_seconds),
                    ("max_bytes", ent.max_bytes),
                    ("result_ttl_hours", ent.result_ttl_hours),
                    ("concurrency", ent.concurrency),
                ] {
                    match value {
                        None => {
                            errors.push(FieldError::new(ent_at(field), "required", "is required"))
                        }
                        Some(n) if n < 0 => errors.push(FieldError::new(
                            ent_at(field),
                            "must_be_non_negative",
                            "must be 0 or more",
                        )),
                        _ => {}
                    }
                }
                match ent.lane.as_deref() {
                    None => errors.push(FieldError::new(ent_at("lane"), "required", "is required")),
                    Some("standard") | Some("priority") => {}
                    Some(_) => errors.push(FieldError::new(
                        ent_at("lane"),
                        "invalid_lane",
                        "must be standard or priority",
                    )),
                }
                match &ent.features {
                    None => errors.push(FieldError::new(
                        ent_at("features"),
                        "required",
                        "is required",
                    )),
                    Some(features) => {
                        for key in features.keys() {
                            if !KNOWN_FEATURES.contains(&key.as_str()) {
                                errors.push(FieldError::new(
                                    ent_at("features"),
                                    "unknown_feature",
                                    format!(
                                        "unknown feature {key} — known features are api_keys, webhooks, batch"
                                    ),
                                ));
                            }
                        }
                    }
                }
            }
        }

        // Free is the resolution fallback: removing it would
        // break every user without a subscription, so the
        // editor refuses it up front (422, not 409 — it is a
        // property of the request, not of the plan's state).
        for (index, code) in remove.iter().enumerate() {
            if code == "free" {
                errors.push(FieldError::new(
                    format!("remove[{index}]"),
                    "free_plan_cannot_be_removed",
                    "the free plan cannot be removed",
                ));
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

/// What one PUT changed, for the response audit detail.
#[derive(Debug, Clone, Default)]
pub struct PlanUpdateSummary {
    /// Codes whose row was updated.
    pub updated: Vec<String>,
    /// Codes whose row was deleted.
    pub removed: Vec<String>,
}

/// The catalog (P1-049): all plans, cheapest first, through
/// the shared cache.
pub async fn list_plans(state: &AppState) -> Result<Vec<plans::Plan>, PlanEditorError> {
    state.plans.catalog().await.map_err(|err| {
        tracing::error!(error = ?err, "admin plans: catalog read failed");
        PlanEditorError::Internal
    })
}

/// Apply one PUT in a single transaction: validate (the
/// handler's `ValidatedJson` already ran — this is the
/// DB-dependent half), then updates, then removals, then
/// commit. Any guard failure rolls the whole PUT back, so a
/// blocked removal never leaves a half-applied edit behind.
/// The caller invalidates the catalog after the commit.
pub async fn update_plans(
    state: &AppState,
    req: &UpdatePlansRequest,
) -> Result<PlanUpdateSummary, PlanEditorError> {
    // Validate (again — the handler's ValidatedJson is the
    // gate; this is the same check for direct callers).
    req.validate().map_err(PlanEditorError::Validation)?;
    let edits = req.plans.clone().unwrap_or_default();
    let remove = req.remove.clone().unwrap_or_default();
    let timeout = PoolConfig::default().transaction_timeout;
    let summary = tx::with_tx(&state.pool, timeout, |conn| {
        let edits = edits.clone();
        let remove = remove.clone();
        Box::pin(async move {
            let mut summary = PlanUpdateSummary::default();
            for edit in &edits {
                let current = plans::by_code(&mut *conn, &edit.code)
                    .await
                    .map_err(PlanTxError::Db)?
                    .ok_or(PlanTxError::NotFound)?;
                let amount = edit
                    .amount
                    .map(|amount| amount as i32)
                    .unwrap_or(current.amount);
                let currency = edit.currency.clone().unwrap_or(current.currency.clone());
                let stripe_price_id = match &edit.stripe_price_id {
                    None => current.stripe_price_id.clone(),
                    Some(cleared) => cleared.clone(),
                };
                let entitlements = match &edit.entitlements {
                    None => current.entitlements.clone(),
                    Some(ent) => serde_json::json!({
                        "daily_images": ent.daily_images,
                        "daily_video_seconds": ent.daily_video_seconds,
                        "max_bytes": ent.max_bytes,
                        "result_ttl_hours": ent.result_ttl_hours,
                        "concurrency": ent.concurrency,
                        "lane": ent.lane,
                        "features": ent.features,
                    }),
                };
                plans::update_by_code(
                    &mut *conn,
                    &edit.code,
                    amount,
                    &currency,
                    stripe_price_id.as_deref(),
                    &entitlements,
                )
                .await
                .map_err(PlanTxError::Db)?
                .ok_or(PlanTxError::NotFound)?;
                summary.updated.push(edit.code.clone());
            }
            for code in &remove {
                let plan = plans::by_code(&mut *conn, code)
                    .await
                    .map_err(PlanTxError::Db)?
                    .ok_or(PlanTxError::NotFound)?;
                // The removal guard (ASSUMPTION-022): any
                // subscription row blocks the delete — live
                // ones because the plan is in use, historical
                // ones because `plan_id` is ON DELETE
                // RESTRICT and the DB would refuse the row.
                let live = subscriptions::count_live_for_plan(&mut *conn, plan.id)
                    .await
                    .map_err(PlanTxError::Db)?;
                if live > 0 {
                    tracing::warn!(
                        plan = code.as_str(),
                        live_subscriptions = live,
                        "admin plans: removal refused — plan has live subscriptions"
                    );
                    return Err(PlanTxError::HasSubscriptions);
                }
                let all = subscriptions::count_all_for_plan(&mut *conn, plan.id)
                    .await
                    .map_err(PlanTxError::Db)?;
                if all > 0 {
                    tracing::warn!(
                        plan = code.as_str(),
                        subscriptions = all,
                        "admin plans: removal refused — plan has historical subscriptions"
                    );
                    return Err(PlanTxError::HasSubscriptions);
                }
                let deleted = plans::delete_by_code(&mut *conn, code)
                    .await
                    .map_err(PlanTxError::Db)?;
                if !deleted {
                    return Err(PlanTxError::NotFound);
                }
                summary.removed.push(code.clone());
            }
            Ok(summary)
        })
    })
    .await
    .map_err(|err| match err {
        TxError::Inner(PlanTxError::NotFound) => PlanEditorError::NotFound,
        TxError::Inner(PlanTxError::HasSubscriptions) => PlanEditorError::HasSubscriptions,
        TxError::Inner(PlanTxError::Db(db_err)) => {
            tracing::error!(error = ?db_err, "admin plans: transaction failed (db)");
            PlanEditorError::Internal
        }
        other => {
            tracing::error!(error = ?other, "admin plans: transaction failed");
            PlanEditorError::Internal
        }
    })?;
    Ok(summary)
}
