//! Role + permission matrix (P1-042, FR-A-07).
//!
//! The single source of truth for who may do what on the admin
//! surface: one [`Capability`] per admin operation, each with a
//! stable code string (`"admin.users.read"`) and the roles that
//! may use it. The guards in [`crate::auth`] (`AdminContext`,
//! `AdminReadContext`) consult this matrix — they look the
//! route up in [`Capability::for_route`] and enforce the row —
//! so the table below, not the extractor type, decides who
//! gets in. Change a row here and both the guard behaviour
//! and the test registry follow; change neither anywhere else.
//!
//! # The table
//!
//! Roles are `user`, `support` (read-only), `admin` (FR-A-07).
//! Every read capability admits admin + support; every write
//! capability and impersonation admit admin only. A plain user
//! and an anonymous caller hold no admin capability at all.
//! Two rules ride on top of the role table, for every row:
//! - an API key never reaches an admin route — even a key
//!   minted by an admin user ("no admin ever", P1-040);
//! - an impersonation token never reaches an admin route —
//!   the token acts as its target user (P1-010).
//!
//! # Routes vs stubs
//!
//! The matrix covers all 28 admin routes in the route table,
//! live or stub. The 21 live routes are enforced by the guards
//! today and walked live by the test registry
//! (`apps/api/tests/authz_matrix.rs`). The 7 stub routes
//! take the guard too since P1-043 (`admin_stub` /
//! `admin_read_stub` — guard first, 501 after), so their rows
//! are enforced promises: the registry's completeness test
//! pins that the row exists, the live walk asserts the guard
//! (401 / 403 / 501) on every stub, and the handler ticket
//! replaces the stub with the real handler keeping the same
//! guard type. The full rendered matrix is the admin guide
//! (`docs/06-DELIVERY/07-admin-guide.md`, P1-047).
//!
//! # Denial shape
//!
//! A caller who fails the matrix gets 403 `PF-1002` in the
//! problem+json envelope (via [`AppError::Forbidden`]) — the
//! same code as every other authorization denial, so a client
//! cannot tell a wrong role from a key or an impersonator.

use crate::auth::{AuthContext, Role};
use axum::http::Method;
use detag_core::error::AppError;

/// One admin operation, as a code constant.
///
/// The code string is the stable identifier: it is what the
/// docs, the audit trail and the test registry name. Add a
/// capability here (never a bare string elsewhere) when a new
/// admin operation lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Capability {
    /// `GET /v1/admin/metrics/dashboard`.
    AdminDashboardRead,
    /// `GET /v1/admin/users` + `GET /v1/admin/users/{id}`
    /// (P1-106).
    AdminUsersRead,
    /// `POST /v1/admin/users/{id}/suspend`,
    /// `/restore`, `/warn`, `/terminate`, `/quota-grant`,
    /// `/notes`.
    AdminUsersWrite,
    /// `POST /v1/admin/users/{id}/impersonate`.
    AdminUsersImpersonate,
    /// `GET /v1/admin/jobs`.
    AdminJobsRead,
    /// `POST /v1/admin/jobs/{id}/reprocess`, `/cancel`.
    AdminJobsWrite,
    /// `GET /v1/admin/dlq`.
    AdminDlqRead,
    /// `GET /v1/admin/flags`.
    AdminFlagsRead,
    /// `PUT /v1/admin/flags`.
    AdminFlagsWrite,
    /// `GET /v1/admin/plans`.
    AdminPlansRead,
    /// `PUT /v1/admin/plans`.
    AdminPlansWrite,
    /// `POST /v1/admin/billing/catalog-sync` (P1-061).
    AdminBillingWrite,
    /// `GET /v1/admin/usage/{id}` + `GET
    /// /v1/admin/usage/{id}/export` (P1-081).
    AdminUsageRead,
    /// `GET /v1/admin/audit`.
    AdminAuditRead,
    /// `GET /v1/admin/abuse-reports`.
    AdminAbuseRead,
    /// `POST /v1/admin/abuse-reports/{id}/resolve`.
    AdminAbuseWrite,
    /// `GET /v1/admin/attestations`.
    AdminAttestationsRead,
    /// `GET /v1/admin/system`.
    AdminSystemRead,
}

impl Capability {
    /// The stable code string for this capability.
    pub fn code(&self) -> &'static str {
        match self {
            Capability::AdminDashboardRead => "admin.dashboard.read",
            Capability::AdminUsersRead => "admin.users.read",
            Capability::AdminUsersWrite => "admin.users.write",
            Capability::AdminUsersImpersonate => "admin.users.impersonate",
            Capability::AdminJobsRead => "admin.jobs.read",
            Capability::AdminJobsWrite => "admin.jobs.write",
            Capability::AdminDlqRead => "admin.dlq.read",
            Capability::AdminFlagsRead => "admin.flags.read",
            Capability::AdminFlagsWrite => "admin.flags.write",
            Capability::AdminPlansRead => "admin.plans.read",
            Capability::AdminPlansWrite => "admin.plans.write",
            Capability::AdminBillingWrite => "admin.billing.write",
            Capability::AdminUsageRead => "admin.usage.read",
            Capability::AdminAuditRead => "admin.audit.read",
            Capability::AdminAbuseRead => "admin.abuse.read",
            Capability::AdminAbuseWrite => "admin.abuse.write",
            Capability::AdminAttestationsRead => "admin.attestations.read",
            Capability::AdminSystemRead => "admin.system.read",
        }
    }

    /// The roles that may use this capability, in
    /// privilege order. Reads admit admin + support;
    /// writes and impersonation admit admin only.
    pub fn allowed_roles(&self) -> &'static [Role] {
        match self {
            Capability::AdminDashboardRead
            | Capability::AdminUsersRead
            | Capability::AdminJobsRead
            | Capability::AdminDlqRead
            | Capability::AdminFlagsRead
            | Capability::AdminPlansRead
            | Capability::AdminUsageRead
            | Capability::AdminAuditRead
            | Capability::AdminAbuseRead
            | Capability::AdminAttestationsRead
            | Capability::AdminSystemRead => &[Role::Admin, Role::Support],
            Capability::AdminUsersWrite
            | Capability::AdminUsersImpersonate
            | Capability::AdminJobsWrite
            | Capability::AdminFlagsWrite
            | Capability::AdminPlansWrite
            | Capability::AdminBillingWrite
            | Capability::AdminAbuseWrite => &[Role::Admin],
        }
    }

    /// Every capability in the matrix, in code order.
    pub fn all() -> &'static [Capability] {
        &[
            Capability::AdminDashboardRead,
            Capability::AdminUsersRead,
            Capability::AdminUsersWrite,
            Capability::AdminUsersImpersonate,
            Capability::AdminJobsRead,
            Capability::AdminJobsWrite,
            Capability::AdminDlqRead,
            Capability::AdminFlagsRead,
            Capability::AdminFlagsWrite,
            Capability::AdminPlansRead,
            Capability::AdminPlansWrite,
            Capability::AdminBillingWrite,
            Capability::AdminUsageRead,
            Capability::AdminAuditRead,
            Capability::AdminAbuseRead,
            Capability::AdminAbuseWrite,
            Capability::AdminAttestationsRead,
            Capability::AdminSystemRead,
        ]
    }

    /// The capability a route needs, by method + route
    /// template (the `MatchedPath` the router matched —
    /// `{id}` stays a template, never a concrete id).
    /// `None` means the route is not an admin route: the
    /// guards fail closed on it (a non-admin route must
    /// never be wired to an admin guard by mistake).
    pub fn for_route(method: &Method, path: &str) -> Option<Capability> {
        use Capability::*;
        match (method.as_str(), path) {
            ("GET", "/v1/admin/metrics/dashboard") => Some(AdminDashboardRead),
            ("GET", "/v1/admin/users") => Some(AdminUsersRead),
            ("GET", "/v1/admin/users/{id}") => Some(AdminUsersRead),
            ("GET", "/v1/admin/users/lookup") => Some(AdminUsersRead),
            ("POST", "/v1/admin/users/{id}/suspend")
            | ("POST", "/v1/admin/users/{id}/restore")
            | ("POST", "/v1/admin/users/{id}/warn")
            | ("POST", "/v1/admin/users/{id}/terminate")
            | ("POST", "/v1/admin/users/{id}/quota-grant")
            | ("POST", "/v1/admin/users/{id}/notes") => Some(AdminUsersWrite),
            ("POST", "/v1/admin/users/{id}/impersonate") => Some(AdminUsersImpersonate),
            ("GET", "/v1/admin/jobs") => Some(AdminJobsRead),
            ("POST", "/v1/admin/jobs/{id}/reprocess") | ("POST", "/v1/admin/jobs/{id}/cancel") => {
                Some(AdminJobsWrite)
            }
            ("GET", "/v1/admin/dlq") => Some(AdminDlqRead),
            ("GET", "/v1/admin/flags") => Some(AdminFlagsRead),
            ("PUT", "/v1/admin/flags") => Some(AdminFlagsWrite),
            ("GET", "/v1/admin/plans") => Some(AdminPlansRead),
            ("PUT", "/v1/admin/plans") => Some(AdminPlansWrite),
            ("POST", "/v1/admin/billing/catalog-sync") => Some(AdminBillingWrite),
            ("POST", "/v1/admin/billing/subscriptions/{id}/cancel") => Some(AdminBillingWrite),
            ("POST", "/v1/admin/billing/subscriptions/{id}/refund") => Some(AdminBillingWrite),
            ("GET", "/v1/admin/usage/{id}") => Some(AdminUsageRead),
            ("GET", "/v1/admin/usage/{id}/export") => Some(AdminUsageRead),
            ("GET", "/v1/admin/audit") => Some(AdminAuditRead),
            ("GET", "/v1/admin/abuse-reports") => Some(AdminAbuseRead),
            ("POST", "/v1/admin/abuse-reports/{id}/resolve") => Some(AdminAbuseWrite),
            ("GET", "/v1/admin/attestations") => Some(AdminAttestationsRead),
            ("GET", "/v1/admin/system") => Some(AdminSystemRead),
            _ => None,
        }
    }

    /// The audit action for an admin WRITE route, by method +
    /// route template (P1-046, FR-A-07): the audit trail's
    /// action name for the privileged action the route
    /// performs. `None` for reads (reads are not audited —
    /// the trail records actions, not views) and for
    /// non-admin routes.
    ///
    /// The action is per ROUTE, not per capability: suspend
    /// / restore / quota-grant / notes share `AdminUsersWrite` but
    /// audit as `user.suspended` / `user.restored` /
    /// `admin.users.quota_granted` / `admin.users.note_added`. The live names are the
    /// ones the services already write (P1-008, P1-010), so
    /// a middleware row and a service row for the same action
    /// are indistinguishable by action — they differ only in
    /// payload richness. A new admin write route gets its
    /// action here, in the same change as its matrix row —
    /// and the audit middleware (`crate::audit_middleware`)
    /// audits it from this table alone, so no handler can
    /// forget its audit row.
    pub fn audit_action_for(method: &Method, path: &str) -> Option<&'static str> {
        match (method.as_str(), path) {
            ("POST", "/v1/admin/users/{id}/suspend") => Some("user.suspended"),
            ("POST", "/v1/admin/users/{id}/restore") => Some("user.restored"),
            ("POST", "/v1/admin/users/{id}/warn") => Some("user.warned"),
            ("POST", "/v1/admin/users/{id}/terminate") => Some("user.terminated"),
            ("POST", "/v1/admin/users/{id}/quota-grant") => Some("admin.users.quota_granted"),
            ("POST", "/v1/admin/users/{id}/notes") => Some("admin.users.note_added"),
            ("POST", "/v1/admin/users/{id}/impersonate") => Some("user.impersonated"),
            ("POST", "/v1/admin/jobs/{id}/reprocess") => Some("admin.jobs.reprocessed"),
            ("POST", "/v1/admin/jobs/{id}/cancel") => Some("admin.jobs.cancelled"),
            ("PUT", "/v1/admin/flags") => Some("admin.flags.updated"),
            ("PUT", "/v1/admin/plans") => Some("admin.plans.updated"),
            ("POST", "/v1/admin/billing/catalog-sync") => Some("admin.billing.catalog_synced"),
            ("POST", "/v1/admin/billing/subscriptions/{id}/cancel") => {
                Some("billing.subscription.status_changed")
            }
            ("POST", "/v1/admin/billing/subscriptions/{id}/refund") => {
                Some("billing.refund.requested")
            }
            ("POST", "/v1/admin/abuse-reports/{id}/resolve") => {
                Some("admin.abuse_reports.resolved")
            }
            _ => None,
        }
    }

    /// The audit target type for an admin write route, by
    /// method + route template (P1-046): the `target_type`
    /// column the middleware writes. Mirrors
    /// [`Capability::audit_action_for`] — `None` exactly
    /// when that is `None`.
    pub fn audit_target_type_for(method: &Method, path: &str) -> Option<&'static str> {
        match (method.as_str(), path) {
            ("POST", "/v1/admin/users/{id}/suspend")
            | ("POST", "/v1/admin/users/{id}/restore")
            | ("POST", "/v1/admin/users/{id}/warn")
            | ("POST", "/v1/admin/users/{id}/terminate")
            | ("POST", "/v1/admin/users/{id}/quota-grant")
            | ("POST", "/v1/admin/users/{id}/notes")
            | ("POST", "/v1/admin/users/{id}/impersonate") => Some("user"),
            ("POST", "/v1/admin/jobs/{id}/reprocess") | ("POST", "/v1/admin/jobs/{id}/cancel") => {
                Some("job")
            }
            ("PUT", "/v1/admin/flags") => Some("flags"),
            ("PUT", "/v1/admin/plans") => Some("plans"),
            ("POST", "/v1/admin/billing/catalog-sync") => Some("plans"),
            ("POST", "/v1/admin/billing/subscriptions/{id}/cancel") => Some("subscription"),
            ("POST", "/v1/admin/billing/subscriptions/{id}/refund") => Some("subscription"),
            ("POST", "/v1/admin/abuse-reports/{id}/resolve") => Some("abuse_report"),
            _ => None,
        }
    }
}

/// The matrix decision: may `role` use `capability`?
///
/// The single source — the guards and the test registry
/// both call this, never a hand-written role comparison.
pub fn role_has_capability(role: Role, capability: Capability) -> bool {
    capability.allowed_roles().contains(&role)
}

/// Enforce one capability against a verified context: the
/// guard core both admin extractors call.
///
/// The checks, in order — a verified JWT caller whose role
/// the capability admits passes; anything else is 403
/// `PF-1002`:
/// - an API-key caller is forbidden on every capability
///   ("no admin ever", P1-040 — the extractor's scope table
///   already rejects keys on admin routes, this is the
///   second line);
/// - an impersonation token is forbidden (P1-010 — the token
///   acts as its target user);
/// - an unverified context fails closed (a handler never
///   sees one — the extractor rejects it first — but the
///   guard does not assume that);
/// - a role the capability does not admit is forbidden.
pub fn require_capability(auth: &AuthContext, capability: Capability) -> Result<(), AppError> {
    if auth.api_key.is_some() {
        return Err(AppError::Forbidden);
    }
    if auth.impersonator_id.is_some() {
        return Err(AppError::Forbidden);
    }
    if !auth.verified {
        return Err(AppError::Forbidden);
    }
    if role_has_capability(auth.role, capability) {
        Ok(())
    } else {
        Err(AppError::Forbidden)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{Credential, VerifiedApiKey};
    use uuid::Uuid;

    fn ctx(role: Role, api_key: bool, impersonator: bool) -> AuthContext {
        AuthContext {
            credential: Credential::Bearer("token".to_string()),
            role,
            verified: true,
            user: None,
            api_key: api_key.then(|| VerifiedApiKey {
                id: Uuid::now_v7(),
                scopes: vec!["jobs:read".to_string()],
                rate_limit_tier: None,
            }),
            impersonator_id: impersonator.then(Uuid::now_v7),
        }
    }

    #[test]
    fn matrix_reads_admit_admin_and_support_only() {
        // Every read capability: admin + support pass, user
        // and anonymous fail. Every write capability and
        // impersonation: admin only.
        for cap in Capability::all() {
            let is_read = cap.code().ends_with(".read");
            assert!(
                role_has_capability(Role::Admin, *cap),
                "{}: admin",
                cap.code()
            );
            assert_eq!(
                role_has_capability(Role::Support, *cap),
                is_read,
                "{}: support (read-only)",
                cap.code()
            );
            assert!(
                !role_has_capability(Role::User, *cap),
                "{}: plain user",
                cap.code()
            );
            assert!(
                !role_has_capability(Role::Anonymous, *cap),
                "{}: anonymous",
                cap.code()
            );
        }
    }

    #[test]
    fn capability_codes_are_unique_and_admin_prefixed() {
        let mut codes = std::collections::HashSet::new();
        for cap in Capability::all() {
            assert!(
                cap.code().starts_with("admin."),
                "{}: code is admin-namespaced",
                cap.code()
            );
            assert!(codes.insert(cap.code()), "{}: duplicate code", cap.code());
        }
        assert_eq!(codes.len(), 18, "one code per capability");
    }

    #[test]
    fn for_route_names_every_admin_route_and_nothing_else() {
        // The route table: 28 admin routes, each with exactly
        // one capability; non-admin routes map to None.
        use axum::http::Method;
        let admin_routes: &[(&str, &str)] = &[
            ("GET", "/v1/admin/metrics/dashboard"),
            ("GET", "/v1/admin/users"),
            ("GET", "/v1/admin/users/{id}"),
            ("GET", "/v1/admin/users/lookup"),
            ("POST", "/v1/admin/users/{id}/suspend"),
            ("POST", "/v1/admin/users/{id}/restore"),
            ("POST", "/v1/admin/users/{id}/warn"),
            ("POST", "/v1/admin/users/{id}/terminate"),
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
            ("POST", "/v1/admin/billing/catalog-sync"),
            ("POST", "/v1/admin/billing/subscriptions/{id}/cancel"),
            ("POST", "/v1/admin/billing/subscriptions/{id}/refund"),
            ("GET", "/v1/admin/usage/{id}"),
            ("GET", "/v1/admin/usage/{id}/export"),
            ("GET", "/v1/admin/audit"),
            ("GET", "/v1/admin/abuse-reports"),
            ("POST", "/v1/admin/abuse-reports/{id}/resolve"),
            ("GET", "/v1/admin/attestations"),
            ("GET", "/v1/admin/system"),
        ];
        assert_eq!(
            admin_routes.len(),
            28,
            "28 admin (method, path) pairs in the route table"
        );
        for (method, path) in admin_routes {
            let m = Method::from_bytes(method.as_bytes()).unwrap();
            assert!(
                Capability::for_route(&m, path).is_some(),
                "{method} {path}: no capability row"
            );
        }
        // Non-admin routes and wrong methods map to None.
        for (method, path) in [
            (Method::GET, "/v1/me"),
            (Method::POST, "/v1/jobs"),
            (Method::POST, "/v1/abuse"),
            (Method::DELETE, "/v1/admin/users"),
            (Method::GET, "/v1/admin"),
        ] {
            assert_eq!(
                Capability::for_route(&method, path),
                None,
                "{method} {path}: must not map"
            );
        }
    }

    #[test]
    fn audit_actions_cover_every_admin_write_and_no_read() {
        // The audit tables ride the route table: every admin
        // write route has exactly one audit action + target
        // type, every admin read route has neither, and
        // non-admin routes have neither. A new admin write
        // route without an audit row fails here — the
        // middleware cannot audit what this table does not
        // name (P1-046).
        use crate::routes::ROUTE_TABLE;
        use axum::http::Method;
        let mut writes = 0;
        let mut reads = 0;
        for (method, path) in ROUTE_TABLE {
            if !path.starts_with("/v1/admin/") {
                assert_eq!(
                    Capability::audit_action_for(
                        &Method::from_bytes(method.as_bytes()).unwrap(),
                        path
                    ),
                    None,
                    "{method} {path}: not an admin route"
                );
                continue;
            }
            let m = Method::from_bytes(method.as_bytes()).unwrap();
            let capability = Capability::for_route(&m, path)
                .unwrap_or_else(|| panic!("{method} {path}: no matrix row"));
            let is_read = capability.code().ends_with(".read");
            let action = Capability::audit_action_for(&m, path);
            let target_type = Capability::audit_target_type_for(&m, path);
            if is_read {
                reads += 1;
                assert_eq!(action, None, "{method} {path}: reads are not audited");
                assert_eq!(target_type, None, "{method} {path}: reads have no target");
            } else {
                writes += 1;
                assert!(
                    action.is_some(),
                    "{method} {path}: write without an audit action"
                );
                assert!(
                    target_type.is_some(),
                    "{method} {path}: write without an audit target type"
                );
            }
        }
        assert_eq!(writes, 15, "15 admin write routes");
        assert_eq!(reads, 14, "14 admin read routes");
    }

    #[test]
    fn audit_action_names_are_unique() {
        // Two routes auditing under one action would merge
        // distinct privileged actions into one trail entry
        // kind — each write route gets its own name.
        use crate::routes::ROUTE_TABLE;
        use axum::http::Method;
        let mut actions = std::collections::HashSet::new();
        for (method, path) in ROUTE_TABLE {
            if !path.starts_with("/v1/admin/") {
                continue;
            }
            let m = Method::from_bytes(method.as_bytes()).unwrap();
            if let Some(action) = Capability::audit_action_for(&m, path) {
                assert!(
                    actions.insert(action),
                    "{method} {path}: duplicate action {action}"
                );
            }
        }
        assert_eq!(actions.len(), 15, "one action per write route");
    }

    #[test]
    fn require_capability_denies_keys_impersonators_and_wrong_roles() {
        let read = Capability::AdminUsersRead;
        let write = Capability::AdminUsersWrite;
        // Admin passes both; support passes the read only.
        assert!(require_capability(&ctx(Role::Admin, false, false), read).is_ok());
        assert!(require_capability(&ctx(Role::Admin, false, false), write).is_ok());
        assert!(require_capability(&ctx(Role::Support, false, false), read).is_ok());
        assert!(matches!(
            require_capability(&ctx(Role::Support, false, false), write),
            Err(AppError::Forbidden)
        ));
        // A plain user fails both.
        assert!(matches!(
            require_capability(&ctx(Role::User, false, false), read),
            Err(AppError::Forbidden)
        ));
        // An admin's own API key fails both ("no admin ever").
        assert!(matches!(
            require_capability(&ctx(Role::Admin, true, false), read),
            Err(AppError::Forbidden)
        ));
        // An impersonation token fails both.
        assert!(matches!(
            require_capability(&ctx(Role::Admin, false, true), read),
            Err(AppError::Forbidden)
        ));
        // Unverified fails closed.
        let mut unverified = ctx(Role::Admin, false, false);
        unverified.verified = false;
        assert!(matches!(
            require_capability(&unverified, read),
            Err(AppError::Forbidden)
        ));
    }
}
