//! The shared extractor set (P0-064).
//!
//! Every route module takes its handler inputs from here —
//! never a module-local extractor type. The set today:
//!
//! - [`State`]`<`[`AppState`]`>` — the process state (pool,
//!   redis, storage, settings, clock). Every handler that
//!   touches a dependency takes it.
//! - `Extension<`[`RequestId`]`>` — the request id the
//!   request-id middleware assigned (and inserted into the
//!   request extensions). Handlers that render an error body
//!   take it so the body carries the same id as the response
//!   header.
//! - [`Path`] / [`Query`] — axum's own path-param and
//!   query-string extractors, re-exported so modules import
//!   extractors from one place.
//!
//! Phase 1 adds the auth extractors here (`CurrentUser`,
//! `AdminUser`, …): a handler that needs the caller takes them
//! from this module, and the middleware that fills them inserts
//! them the same way the request-id middleware inserts
//! `RequestId` today. Do not invent a per-module auth
//! extractor in the meantime.

pub use crate::request_id::RequestId;
pub use crate::AppState;
pub use axum::extract::{Path, Query, State};
