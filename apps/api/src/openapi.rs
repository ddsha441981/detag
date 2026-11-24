//! OpenAPI scaffold (P0-067, FR-X-01): the generated spec, the
//! `/openapi.json` document, and the Swagger UI at `/docs`.
//!
//! The spec is generated from code (utoipa derives), never written
//! by hand: [`ApiDoc::openapi()`] is the single source, served at
//! `/openapi.json` and committed to `docs/api/openapi.json` by
//! `just openapi`. `just openapi-check` (and CI) fails when the
//! committed spec drifts from the code — regenerate, never edit
//! the JSON by hand.
//!
//! Coverage today is the forty-three real handlers (`/healthz`,
//! `/readyz`, `/v1/auth/register`, `/v1/auth/verify-email`,
//! `/v1/auth/resend-verification`, `/v1/auth/login`,
//! `/v1/auth/refresh`, `/v1/auth/logout`,
//! `/v1/auth/logout-all`, `GET /v1/me`,
//! `PATCH /v1/me`, `POST /v1/auth/confirm-email-change`,
//! `DELETE /v1/me`, `GET /v1/me/sessions`,
//! `DELETE /v1/me/sessions/{id}`,
//! `POST /v1/auth/forgot-password`,
//! `POST /v1/auth/reset-password`,
//! `POST /v1/admin/users/{id}/suspend`,
//! `POST /v1/admin/users/{id}/restore`,
//! `POST /v1/admin/users/{id}/warn`,
//! `POST /v1/admin/users/{id}/terminate`, `GET /v1/admin/users`,
//! `POST /v1/admin/users/{id}/impersonate`,
//! `GET /v1/admin/jobs`, `GET /v1/admin/plans`,
//! `PUT /v1/admin/plans`, `POST
//! /v1/admin/billing/catalog-sync`, `POST
//! /v1/admin/billing/subscriptions/{id}/cancel`, `GET
//! /v1/admin/usage/{id}`, `GET
//! /v1/admin/usage/{id}/export`, `GET /v1/admin/audit`,
//! `POST /v1/abuse`, `POST /v1/billing/checkout`, `POST /v1/billing/portal`, `GET
//! /v1/billing/invoices`, `GET /v1/usage`, `POST /v1/webhooks/stripe`, `POST
//! /v1/webhooks/razorpay` — the Razorpay receiver is flag-gated
//! (P1-073): 404 while the flag is off, 501 until P1-074 wires
//! it)
//! plus the shared error schema (`Problem`).
//! The 30 stub routes are NOT
//! in the spec yet: each endpoint ticket adds its
//! `#[utoipa::path]` annotation and its path to this
//! `#[openapi]` list when its handler becomes real (see the
//! repo-layout doc: "New endpoint: routes/ + service + repo +
//! OpenAPI derive + integration test"). A spec that listed 47
//! stubs would document endpoints that do not exist.
//!
//! `/openapi.json` is public in every environment — the console
//! viewer (P4-064) fetches it in prod too. Only the interactive
//! UI (`/docs`) is gated: dev and stage only, never prod
//! (`Settings::docs_enabled`, `PF_ENVIRONMENT`).
//!
//! `/openapi.json` and `/docs` are meta routes outside
//! `ROUTE_TABLE` (which lists the `/v1` API-spec routes); they
//! are covered by `apps/api/tests/openapi.rs` instead.

use axum::{response::Json, Router};
use detag_core::db::{ComponentHealth, Readiness};
use detag_core::error::Problem;
use utoipa::OpenApi;
use utoipa_swagger_ui::{Config, SwaggerUi, Url};

use crate::routes::admin::{
    __path_abuse_intake, __path_admin_abuse_queue_handler, __path_admin_attestations_handler,
    __path_admin_audit_handler, __path_admin_usage_export_handler, __path_admin_usage_handler,
    __path_admin_user_view_handler, __path_cancel_subscription, __path_catalog_sync_handler,
    __path_impersonate, __path_list_jobs_handler, __path_list_plans_handler,
    __path_quota_grant_handler, __path_resolve_abuse_report_handler, __path_restore,
    __path_search_users_handler, __path_suspend, __path_terminate_user_handler,
    __path_update_plans_handler, __path_warn_user_handler,
};
use crate::routes::api_keys::{
    __path_create_api_key, __path_revoke_api_key, __path_rotate_api_key,
};
use crate::routes::auth::{
    __path_confirm_email_change, __path_forgot_password, __path_login, __path_logout,
    __path_logout_all, __path_refresh, __path_register, __path_resend_verification,
    __path_reset_password, __path_verify_email,
};
use crate::routes::billing::{
    __path_create_checkout, __path_create_portal, __path_get_usage, __path_list_invoices,
    __path_receive_razorpay_webhook, __path_receive_stripe_webhook,
};
use crate::routes::meta::{__path_healthz, __path_readyz};
use crate::routes::profile::{
    __path_accept_consent, __path_delete_me, __path_get_email_preferences, __path_get_me,
    __path_get_onboarding, __path_patch_me, __path_put_email_preferences,
};
use crate::routes::sessions::{__path_delete_session, __path_list_sessions};
use crate::services::abuse_intake::{AbuseIntakeRequest, AbuseReportResponse};
use crate::services::admin::{
    AdminJobItem, AdminJobsPage, ImpersonateResponse, ReasonRequest, UserSearchItem, UserSearchPage,
};
use crate::services::admin_abuse_queue::{
    AbuseQueuePage, AbuseReportItem, ResolveAbuseReportRequest,
};
use crate::services::admin_attestations::{AttestationItem, AttestationsPage};
use crate::services::admin_audit::{AuditLogItem, AuditPage};
use crate::services::admin_plans::{
    PlanEdit, PlanEntitlementsEdit, PlanView, PlansResponse, UpdatePlansRequest,
};
use crate::services::admin_usage::{AdminUsageResponse, UsageTotals};
use crate::services::admin_user_view::{
    AdminUserViewResponse, RecentJobView, UserPlanView, UserProfileView, UserUsageView,
};
use crate::services::api_keys::{CreateApiKeyRequest, CreateApiKeyResponse};
use crate::services::checkout::{CheckoutRequestBody, CheckoutResponse, ScheduledChangeResponse};
use crate::services::consent::{AcceptConsentRequest, ConsentStatus};
use crate::services::email_prefs::{EmailPrefs, UpdateEmailPreferencesRequest};
use crate::services::enforcement::{EnforcementResponse, EnforcementStep};
use crate::services::login::{LoginRequest, LoginResponse};
use crate::services::onboarding::{OnboardingChecklist, OnboardingStep};
use crate::services::password_reset::{ForgotPasswordRequest, ResetPasswordRequest};
use crate::services::profile::{PatchProfileRequest, ProfileResponse};
use crate::services::quota_grant::{QuotaGrantRequest, QuotaGrantResponse};
use crate::services::register::{RegisterRequest, UserResponse};
use crate::services::sessions::SessionItem;
use crate::services::support_cancel::{RefundGuidance, SupportCancelResponse};
use crate::services::usage::{DayUsage, KeyUsage, UsageResponse};
use crate::services::verification::{
    ConfirmEmailChangeRequest, ResendVerificationRequest, VerifyEmailRequest,
};
use crate::AppState;

/// The API document (utoipa derive). `info.version` is the API
/// version (`/v1`), not the crate version.
#[derive(OpenApi)]
#[openapi(
    info(
        title = "Detag API",
        version = "1.0.0",
        description = "Detag — watermark removal for images and videos you own. All errors are RFC 7807 problem+json with a stable PF-code (see the Problem schema)."
    ),
    paths(healthz, readyz, register, verify_email, resend_verification, confirm_email_change, login, refresh, logout, logout_all, forgot_password, reset_password, get_me, patch_me, delete_me, accept_consent, get_onboarding, get_email_preferences, put_email_preferences, list_sessions, delete_session, create_api_key, rotate_api_key, revoke_api_key, suspend, restore, warn_user_handler, terminate_user_handler, quota_grant_handler, search_users_handler, admin_user_view_handler, list_jobs_handler, impersonate, list_plans_handler, update_plans_handler, catalog_sync_handler, cancel_subscription, admin_usage_handler, admin_usage_export_handler, admin_audit_handler, admin_abuse_queue_handler, resolve_abuse_report_handler, admin_attestations_handler, abuse_intake, create_checkout, create_portal, list_invoices, get_usage, receive_stripe_webhook, receive_razorpay_webhook),
    components(schemas(
        Problem,
        Readiness,
        ComponentHealth,
        RegisterRequest,
        UserResponse,
        VerifyEmailRequest,
        ResendVerificationRequest,
        ConfirmEmailChangeRequest,
        ForgotPasswordRequest,
        ResetPasswordRequest,
        LoginRequest,
        LoginResponse,
        PatchProfileRequest,
        ProfileResponse,
        AcceptConsentRequest,
        ConsentStatus,
        OnboardingChecklist,
        OnboardingStep,
        EmailPrefs,
        UpdateEmailPreferencesRequest,
        SessionItem,
        CreateApiKeyRequest,
        CreateApiKeyResponse,
        ReasonRequest,
        UserSearchItem,
        UserSearchPage,
        AdminJobItem,
        AdminJobsPage,
        ImpersonateResponse,
        PlansResponse,
        PlanView,
        UpdatePlansRequest,
        PlanEdit,
        PlanEntitlementsEdit,
        CheckoutRequestBody,
        CheckoutResponse,
        ScheduledChangeResponse,
        SupportCancelResponse,
        RefundGuidance,
        UsageResponse,
        DayUsage,
        KeyUsage,
        AdminUsageResponse,
        UsageTotals,
        AdminUserViewResponse,
        UserProfileView,
        UserPlanView,
        UserUsageView,
        RecentJobView,
        QuotaGrantRequest,
        QuotaGrantResponse,
        AuditPage,
        AuditLogItem,
        AbuseQueuePage,
        AbuseReportItem,
        ResolveAbuseReportRequest,
        AttestationsPage,
        AttestationItem,
        AbuseIntakeRequest,
        AbuseReportResponse,
        EnforcementResponse,
        EnforcementStep,
        crate::services::webhook::WebhookReceipt,
        detag_billing::CatalogDriftReport,
        detag_billing::PlanSyncResult,
        detag_billing::SyncAction,
        detag_core::plans::Entitlements,
        detag_core::plans::Lane
    )),
    tags(
        (name = "meta", description = "Liveness and readiness"),
        (name = "auth", description = "Registration, sessions, API keys")
    )
)]
pub struct ApiDoc;

/// Serve the generated spec as JSON (always on — see the module
/// docs). `Json<OpenApi>` renders `application/json`; the
/// `application/problem+json` media type belongs to error bodies
/// only, not to this document.
pub(crate) async fn openapi_json() -> Json<utoipa::openapi::OpenApi> {
    Json(ApiDoc::openapi())
}

/// The Swagger UI router for `/docs`. The UI fetches its spec from
/// `/openapi.json` — the config only points at that URL, it does
/// not register the route, so there is exactly one `/openapi.json`
/// handler (the one above) and no duplicate-route panic at router
/// build time. Merge this into the router only when
/// `Settings::docs_enabled()` (dev/stage); prod builds no `/docs`
/// at all.
pub(crate) fn docs_router() -> Router<AppState> {
    let config = Config::new([Url::new("Detag API", "/openapi.json")]);
    Router::<AppState>::from(SwaggerUi::new("/docs").config(config))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The scaffold contract: the spec has the two real paths, the
    /// Problem error schema with its wire fields, and the readyz
    /// response schema with per-component latency.
    #[test]
    fn spec_contains_meta_paths_and_problem_schema() {
        let json = ApiDoc::openapi()
            .to_json()
            .expect("the spec must serialize");
        let spec: serde_json::Value = serde_json::from_str(&json).expect("the spec must be JSON");
        assert_eq!(spec["info"]["title"], "Detag API");
        assert_eq!(spec["info"]["version"], "1.0.0");
        assert!(spec["paths"]["/healthz"].is_object(), "{spec}");
        assert!(spec["paths"]["/readyz"].is_object(), "{spec}");
        assert!(
            spec["paths"]["/readyz"]["get"]["responses"]["200"].is_object(),
            "readyz must document 200: {spec}"
        );
        assert!(
            spec["paths"]["/readyz"]["get"]["responses"]["503"].is_object(),
            "readyz must document 503: {spec}"
        );
        let problem = &spec["components"]["schemas"]["Problem"];
        assert!(problem.is_object(), "Problem schema must exist: {spec}");
        for field in ["type", "title", "status", "code", "detail"] {
            assert!(
                problem["properties"][field].is_object(),
                "Problem must document {field}: {spec}"
            );
        }
        let readiness = &spec["components"]["schemas"]["Readiness"];
        assert!(readiness.is_object(), "Readiness schema must exist: {spec}");
        let component = &spec["components"]["schemas"]["ComponentHealth"];
        assert!(
            component["properties"]["latency_ms"].is_object(),
            "ComponentHealth must document latency_ms: {spec}"
        );
    }
}
