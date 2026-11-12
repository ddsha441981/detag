//! Upload routes (P0-064): `/v1/uploads/*`.
//!
//! All handlers are the shared 501 stub until their Phase 2/3
//! tickets land (intake owns create/complete/abort/get).

use super::stub;
use crate::AppState;
use axum::{
    routing::{get, post},
    Router,
};

/// The uploads nest: create (multipart session), complete
/// (part + whole-object verify, re-sniff), abort, get.
pub fn router() -> Router<AppState> {
    Router::new().nest(
        "/v1/uploads",
        Router::new()
            .route("/", post(stub))
            .route("/{id}", get(stub))
            .route("/{id}/complete", post(stub))
            .route("/{id}/abort", post(stub)),
    )
}
