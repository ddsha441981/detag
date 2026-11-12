//! Auth routes (P0-064): `/v1/auth/*`, `/v1/me/*`, `/v1/api-keys/*`.
//!
//! API keys live here (they are credentials; the spec's six
//! modules leave them no other home). All handlers are the
//! shared 501 stub until their Phase 1 tickets land.

use super::stub;
use crate::AppState;
use axum::{
    routing::{delete, get, post},
    Router,
};

/// The auth nests: registration/login/session management under
/// `/v1/auth`, the profile + sessions under `/v1/me`, API keys
/// under `/v1/api-keys`.
pub fn router() -> Router<AppState> {
    Router::new()
        .nest(
            "/v1/auth",
            Router::new()
                .route("/register", post(stub))
                .route("/verify-email", post(stub))
                .route("/login", post(stub))
                .route("/refresh", post(stub))
                .route("/logout", post(stub))
                .route("/forgot-password", post(stub))
                .route("/reset-password", post(stub)),
        )
        .nest(
            "/v1/me",
            Router::new()
                .route("/", get(stub).patch(stub).delete(stub))
                .route("/sessions", get(stub))
                .route("/sessions/{id}", delete(stub))
                .route("/export", post(stub)),
        )
        .nest(
            "/v1/api-keys",
            Router::new()
                .route("/", get(stub).post(stub))
                .route("/{id}", delete(stub)),
        )
}
