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
//! Coverage today is the twenty-one real handlers (`/healthz`,
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
//! `POST /v1/admin/users/{id}/restore`, `GET /v1/admin/users`,
//! `POST /v1/admin/users/{id}/impersonate`)
//! plus the shared error schema (`Problem`).
//! The 45 stub routes are NOT
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
    __path_impersonate, __path_restore, __path_search_users_handler, __path_suspend,
};
use crate::routes::api_keys::__path_create_api_key;
use crate::routes::auth::{
    __path_confirm_email_change, __path_forgot_password, __path_login, __path_logout,
    __path_logout_all, __path_refresh, __path_register, __path_resend_verification,
    __path_reset_password, __path_verify_email,
};
use crate::routes::meta::{__path_healthz, __path_readyz};
use crate::routes::profile::{__path_delete_me, __path_get_me, __path_patch_me};
use crate::routes::sessions::{__path_delete_session, __path_list_sessions};
use crate::services::admin::{ImpersonateResponse, ReasonRequest, UserSearchItem, UserSearchPage};
use crate::services::api_keys::{CreateApiKeyRequest, CreateApiKeyResponse};
use crate::services::login::{LoginRequest, LoginResponse};
use crate::services::password_reset::{ForgotPasswordRequest, ResetPasswordRequest};
use crate::services::profile::{PatchProfileRequest, ProfileResponse};
use crate::services::register::{RegisterRequest, UserResponse};
use crate::services::sessions::SessionItem;
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
    paths(healthz, readyz, register, verify_email, resend_verification, confirm_email_change, login, refresh, logout, logout_all, forgot_password, reset_password, get_me, patch_me, delete_me, list_sessions, delete_session, create_api_key, suspend, restore, search_users_handler, impersonate),
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
        SessionItem,
        CreateApiKeyRequest,
        CreateApiKeyResponse,
        ReasonRequest,
        UserSearchItem,
        UserSearchPage,
        ImpersonateResponse
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
