//! Privileged-action audit middleware (P1-046, FR-A-07).
//!
//! Route metadata drives automatic audit for admin writes:
//! the middleware sits on the `/v1/admin` nest, looks the
//! route up in [`Capability::audit_action_for`]
//! (`crate::authz`) and, for a successful write, inserts
//! exactly one audit row — actor, action, target — so no
//! handler can forget its audit row. A new admin write
//! route gets its audit from this table alone, in the same
//! change as its matrix row.
//!
//! # What it audits (and what it does not)
//!
//! A 2xx response to an admin WRITE route writes one row:
//! action and target type from the route metadata, target id
//! from the `{id}` path segment when the route has one, actor
//! from the verified credential. Nothing else is audited:
//! reads are not (the trail records actions, not views), and
//! a non-2xx response is not (a denial, a 404, or a 501 stub
//! changed nothing — P1-043's "actions, not attempts" rule,
//! unchanged).
//!
//! # The two audit paths, and why both exist
//!
//! The three live writes (suspend / restore / impersonate)
//! keep the audit insert their services already do: it runs
//! inside the SAME transaction as the mutation (P1-008's
//! atomicity — the row and the status flip commit together
//! or not at all). The middleware runs after the handler,
//! so it cannot join that transaction. Those handlers mark
//! their success response with [`AuditHandled`] and the
//! middleware stands down — the row exists exactly once.
//!
//! Every other admin write is audited here. A handler that
//! writes no row of its own MUST NOT set the marker: the
//! middleware then audits the write from the route metadata.
//! A forgotten manual call can therefore only ever cost
//! payload detail, never the row itself. A handler that
//! wants rich payloads on a middleware-written row attaches
//! [`AuditDetail`] to its response extensions; without it
//! the row carries the action, actor and target, and no
//! before/after.
//!
//! # Failure modes
//!
//! The actor is resolved by running the same credential
//! verification the handler's extractor runs
//! (`AuthContext::from_request_parts`) — a second
//! verification per admin write (one extra user lookup);
//! admin writes are rare, and the guard stays authoritative:
//! the middleware audits only 2xx responses, so a caller
//! the guard rejects (wrong role, API key, impersonator)
//! is never audited however the middleware resolved them.
//! When the credential does not verify at all, the
//! middleware passes the request through unaudited — the
//! handler's extractor rejects it, and the response is not
//! 2xx anyway.
//!
//! When the audit insert itself fails, the middleware
//! answers 500 (the login precedent — a privileged action
//! without its audit row is a failed request, not a silent
//! gap). The mutation has already committed by then, so
//! the client must re-read state rather than blindly retry;
//! the admin writes are idempotent (suspend / restore are
//! no-ops on repeat), so a retry after such a 500 is safe.

use crate::{
    auth::AuthContext, authz::Capability, problem::ProblemResponse, routes::extractors::RequestId,
    AppState,
};
use axum::{
    body::Body,
    extract::{FromRequestParts, MatchedPath, Request, State},
    http::Method,
    middleware::Next,
    response::{IntoResponse, Response},
};
use detag_core::{
    audit::{record, NewAuditLog},
    error::{AppError, Profile},
};
use serde_json::Value;

/// Marker a handler inserts into its response extensions
/// when its service already wrote the audit row for this
/// request — or deliberately wrote none (an idempotent
/// no-op: suspending an already-suspended account writes
/// no row by design). The middleware stands down when it
/// sees this marker: the handler owns its audit decision,
/// and the row exists exactly once. See the module docs.
#[derive(Debug, Clone, Copy)]
pub struct AuditHandled;

/// Optional audit detail a handler attaches to its response
/// extensions when the middleware writes the row: the
/// before/after payloads and the reason. Without it the
/// row carries the action, actor and target, and no
/// before/after — the row itself is never optional.
#[derive(Debug, Clone, Default)]
pub struct AuditDetail {
    /// State before the action, if meaningful.
    pub before: Option<Value>,
    /// State after the action (never PII — hashes, not values).
    pub after: Option<Value>,
    /// Why, when the action needs a reason.
    pub reason: Option<String>,
}

/// The privileged-action audit middleware (P1-046).
///
/// Layered on the `/v1/admin` nest only — the public
/// `/v1/abuse` intake is not an admin route and never
/// reaches this middleware. Reads and non-admin routes pass
/// through untouched; a successful admin write gets exactly
/// one audit row unless the handler marked the response
/// [`AuditHandled`].
pub async fn audit_admin_writes(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let (mut parts, body) = request.into_parts();
    let method = parts.method.clone();
    let template = parts
        .extensions
        .get::<MatchedPath>()
        .map(|matched| matched.as_str().to_string());
    let Some(template) = template else {
        // No matched path (the router never matched): pass
        // through — there is no route metadata to audit from.
        return next.run(Request::from_parts(parts, body)).await;
    };
    let Some(action) = Capability::audit_action_for(&method, &template) else {
        // A read, or a route the audit table does not name:
        // pass through untouched.
        return next.run(Request::from_parts(parts, body)).await;
    };
    // The target type rides the same table — it is `Some`
    // exactly when the action is (pinned by unit test).
    let target_type = Capability::audit_target_type_for(&method, &template)
        .expect("audit action without a target type");
    let target_id = target_id_from(&method, &template, parts.uri.path());
    let request_id = parts.extensions.get::<RequestId>().map(|id| id.0.clone());
    // Resolve the actor the same way the handler's extractor
    // does. A credential that does not verify passes through
    // unaudited — the extractor rejects it, so the response
    // cannot be 2xx. (This is a second verification per admin
    // write; see the module docs.)
    let actor = match AuthContext::from_request_parts(&mut parts, &state).await {
        Ok(auth) => auth.user.as_ref().map(|user| auth.audit_actor_id(user)),
        Err(_) => None,
    };
    let request = Request::from_parts(parts, body);
    let response = next.run(request).await;
    if !response.status().is_success() {
        return response;
    }
    if response.extensions().get::<AuditHandled>().is_some() {
        // The handler's service wrote the row (or
        // deliberately wrote none) — stand down.
        return response;
    }
    let Some(actor_user_id) = actor else {
        // A 2xx without a verified actor cannot happen (the
        // guard admits only verified callers) — but if it
        // ever did, fail closed: a privileged action without
        // an actor in the trail is worse than a 500.
        tracing::error!("audit middleware: 2xx admin write with no verified actor");
        return audit_failure_response(request_id.as_deref());
    };
    let detail = response
        .extensions()
        .get::<AuditDetail>()
        .cloned()
        .unwrap_or_default();
    let insert = record(
        &state.pool,
        &NewAuditLog {
            actor_user_id: Some(actor_user_id),
            actor_client: None,
            action: action.to_string(),
            target_type: target_type.to_string(),
            target_id,
            before: detail.before,
            after: detail.after,
            reason: detail.reason,
        },
    )
    .await;
    if let Err(err) = insert {
        tracing::error!(error = ?err, action, "audit middleware: audit insert failed");
        return audit_failure_response(request_id.as_deref());
    }
    response
}

/// The `{id}` path segment for a route whose template
/// carries one, taken from the concrete request path at the
/// same segment position. `None` for routes without `{id}`
/// (flags, plans) and when the positions do not line up
/// (a malformed path the handler will reject anyway).
fn target_id_from(_method: &Method, template: &str, path: &str) -> Option<String> {
    let id_at = template.split('/').position(|segment| segment == "{id}")?;
    let found = path.split('/').nth(id_at)?;
    if found.is_empty() {
        None
    } else {
        Some(found.to_string())
    }
}

/// The 500 the middleware answers when the audit row cannot
/// be written (or a 2xx admin write has no verified actor):
/// a privileged action without its audit row is a failed
/// request. The mutation has already committed — the client
/// must re-read state rather than blindly retry.
fn audit_failure_response(request_id: Option<&str>) -> Response {
    ProblemResponse(AppError::Internal.to_problem(Profile::Production, request_id, None))
        .into_response()
}

#[allow(clippy::assertions_on_constants)]
#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;

    #[test]
    fn target_id_is_the_segment_at_the_template_position() {
        assert_eq!(
            target_id_from(
                &Method::POST,
                "/v1/admin/users/{id}/suspend",
                "/v1/admin/users/123e4567-e89b-42d3-a456-426614174109/suspend"
            ),
            Some("123e4567-e89b-42d3-a456-426614174109".to_string())
        );
        assert_eq!(
            target_id_from(
                &Method::POST,
                "/v1/admin/abuse-reports/{id}/resolve",
                "/v1/admin/abuse-reports/abc/resolve"
            ),
            Some("abc".to_string())
        );
        // No `{id}` in the template: no target.
        assert_eq!(
            target_id_from(&Method::PUT, "/v1/admin/flags", "/v1/admin/flags"),
            None
        );
        // A path too short to hold the id: no target (the
        // handler rejects such a path before it can be 2xx).
        assert_eq!(
            target_id_from(
                &Method::POST,
                "/v1/admin/users/{id}/suspend",
                "/v1/admin/users"
            ),
            None
        );
    }

    #[test]
    fn audit_failure_is_a_500_problem() {
        let response = audit_failure_response(Some("req-1"));
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
