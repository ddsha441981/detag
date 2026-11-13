//! Route modules (P0-064, E0.7).
//!
//! One module per area — `auth`, `uploads`, `jobs`, `billing`,
//! `admin`, `meta` — each owning its nests under `/v1` (meta
//! owns the unversioned `/healthz` + `/readyz`). [`api_router`]
//! merges them; [`build_router`](crate::build_router) layers
//! the middleware on top (layer order is documented in
//! `crate`'s module docs).
//!
//! # Stubs
//!
//! Every route is a stub until its Phase 1/2 ticket implements
//! it: the shared [`stub`] handler answers 501 `PF-5004`
//! (`AppError::NotImplemented`) in the canonical problem+json
//! envelope, with the request id from the middleware. A route
//! that answers 501 exists; a route that answers 404 does not
//! — that is what the route-table test checks.
//!
//! # The route table
//!
//! [`ROUTE_TABLE`] is the (method, path) table from the API
//! spec (`docs/03-ARCHITECTURE/03-api-spec.md`), 64 entries.
//! It is the test oracle (`apps/api/tests/route_table.rs`):
//! no duplicate (method, path) pairs, and every entry resolves
//! to the 501 stub on the built router. When you add a route,
//! add its entry here too — the probe test fails if a table
//! entry is missing from the router. (The reverse — a route
//! registered but missing from the table — is not caught
//! mechanically; each real handler's own test covers its route
//! when it lands.)
//!
//! Two spec ambiguities, resolved here and recorded so Phase 1
//! does not re-decide them silently:
//! - API keys live in `auth` (they are credentials, and the
//!   spec's six modules leave them no other home).
//! - The abuse-resolve endpoint is
//!   `POST /v1/admin/abuse-reports/{id}/resolve` — the spec
//!   says only "POST resolve with action" under the
//!   abuse-reports row.

pub mod admin;
pub mod auth;
pub mod billing;
pub mod extractors;
pub mod jobs;
pub mod meta;
pub mod profile;
pub mod sessions;
pub mod uploads;

use crate::problem::ProblemResponse;
use axum::{extract::Extension, Router};
use detag_core::error::{AppError, Profile};

use self::extractors::RequestId;
use crate::AppState;

/// The (method, path) table from the API spec. Counts per
/// module: auth 19, uploads 4, jobs 9, billing 12, admin 20,
/// meta 2 — 66 total.
pub const ROUTE_TABLE: &[(&str, &str)] = &[
    // auth (19)
    ("POST", "/v1/auth/register"),
    ("POST", "/v1/auth/verify-email"),
    ("POST", "/v1/auth/resend-verification"),
    ("POST", "/v1/auth/confirm-email-change"),
    ("POST", "/v1/auth/login"),
    ("POST", "/v1/auth/refresh"),
    ("POST", "/v1/auth/logout"),
    ("POST", "/v1/auth/logout-all"),
    ("POST", "/v1/auth/forgot-password"),
    ("POST", "/v1/auth/reset-password"),
    ("GET", "/v1/me"),
    ("PATCH", "/v1/me"),
    ("DELETE", "/v1/me"),
    ("GET", "/v1/me/sessions"),
    ("DELETE", "/v1/me/sessions/{id}"),
    ("POST", "/v1/me/export"),
    ("GET", "/v1/api-keys"),
    ("POST", "/v1/api-keys"),
    ("DELETE", "/v1/api-keys/{id}"),
    // uploads (4)
    ("POST", "/v1/uploads"),
    ("POST", "/v1/uploads/{id}/complete"),
    ("POST", "/v1/uploads/{id}/abort"),
    ("GET", "/v1/uploads/{id}"),
    // jobs (9)
    ("POST", "/v1/jobs"),
    ("GET", "/v1/jobs"),
    ("GET", "/v1/jobs/{id}"),
    ("POST", "/v1/jobs/{id}/cancel"),
    ("POST", "/v1/jobs/{id}/retry"),
    ("GET", "/v1/jobs/{id}/events"),
    ("GET", "/v1/jobs/{id}/result"),
    ("GET", "/v1/jobs/{id}/report"),
    ("POST", "/v1/jobs/{id}/attestation"),
    // billing (12)
    ("GET", "/v1/plans"),
    ("GET", "/v1/billing/subscription"),
    ("POST", "/v1/billing/checkout"),
    ("POST", "/v1/billing/portal"),
    ("GET", "/v1/billing/invoices"),
    ("GET", "/v1/usage"),
    ("POST", "/v1/webhooks/stripe"),
    ("POST", "/v1/webhooks/razorpay"),
    ("GET", "/v1/webhook-endpoints"),
    ("POST", "/v1/webhook-endpoints"),
    ("DELETE", "/v1/webhook-endpoints/{id}"),
    ("GET", "/v1/webhook-endpoints/{id}/deliveries"),
    // admin (20)
    ("GET", "/v1/admin/metrics/dashboard"),
    ("GET", "/v1/admin/users"),
    ("POST", "/v1/admin/users/{id}/suspend"),
    ("POST", "/v1/admin/users/{id}/restore"),
    ("POST", "/v1/admin/users/{id}/quota-grant"),
    ("POST", "/v1/admin/users/{id}/impersonate"),
    ("GET", "/v1/admin/jobs"),
    ("POST", "/v1/admin/jobs/{id}/reprocess"),
    ("POST", "/v1/admin/jobs/{id}/cancel"),
    ("GET", "/v1/admin/dlq"),
    ("GET", "/v1/admin/flags"),
    ("PUT", "/v1/admin/flags"),
    ("GET", "/v1/admin/plans"),
    ("PUT", "/v1/admin/plans"),
    ("GET", "/v1/admin/audit"),
    ("GET", "/v1/admin/abuse-reports"),
    ("POST", "/v1/admin/abuse-reports/{id}/resolve"),
    ("GET", "/v1/admin/attestations"),
    ("GET", "/v1/admin/system"),
    ("POST", "/v1/abuse"),
    // meta (2)
    ("GET", "/healthz"),
    ("GET", "/readyz"),
];

/// Merge every module's nests into one router. A duplicate or
/// overlapping route panics here at build time (axum) — the
/// route-table test relies on that: building the router IS the
/// duplicate check, and the probe test is the presence check.
///
/// Body limit (P0-065, per-class caps P0-104): every module
/// except uploads carries the limit layer of its class — auth +
/// meta `Small` (64 KiB), jobs + admin `Standard` (1 MiB),
/// billing `Webhook` (256 KiB). Uploads are exempt structurally
/// — upload bytes never transit the api (presigned S3 PUTs), and
/// the nest without a limit layer cannot be capped by accident
/// by a future streaming endpoint.
pub fn api_router() -> Router<AppState> {
    Router::new()
        .merge(
            auth::router().layer(crate::body_limit::body_limit_layer_for(
                crate::body_limit::BodyLimitClass::Small,
            )),
        )
        .merge(uploads::router())
        .merge(
            jobs::router().layer(crate::body_limit::body_limit_layer_for(
                crate::body_limit::BodyLimitClass::Standard,
            )),
        )
        .merge(
            billing::router().layer(crate::body_limit::body_limit_layer_for(
                crate::body_limit::BodyLimitClass::Webhook,
            )),
        )
        .merge(
            admin::router().layer(crate::body_limit::body_limit_layer_for(
                crate::body_limit::BodyLimitClass::Standard,
            )),
        )
        .merge(
            meta::router().layer(crate::body_limit::body_limit_layer_for(
                crate::body_limit::BodyLimitClass::Small,
            )),
        )
}

/// The shared Phase 0 stub: 501 `PF-5004` in the problem+json
/// envelope, rendered through [`ProblemResponse`](crate::problem::ProblemResponse)
/// so the media type is `application/problem+json`, not plain
/// `application/json`. The `Production` profile is deliberate — a
/// stub carries no caller detail, so the static safe detail is the
/// whole body in every environment. The request id comes from
/// the middleware via `Extension<RequestId>`.
async fn stub(Extension(id): Extension<RequestId>) -> ProblemResponse {
    let problem = AppError::NotImplemented.to_problem(Profile::Production, Some(&id.0), None);
    ProblemResponse(problem)
}
