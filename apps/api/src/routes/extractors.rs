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
//! - [`AuthContext`] — who the caller is (P0-070): which
//!   credential was presented (Bearer JWT, API key, or web
//!   session cookie) and the caller's [`Role`]. Phase 0 parses
//!   but does NOT verify — `verified` is always `false` and the
//!   role always [`Role::Anonymous`]; Phase 1 adds verification
//!   and the 401 rejections. See `crate::auth` for the full
//!   Phase 0 contract before using it.
//!
//! Do not invent a per-module auth extractor: a handler that
//! needs the caller takes [`AuthContext`] from this module.

pub use crate::auth::{AuthContext, Credential, Role};
pub use crate::request_id::RequestId;
pub use crate::AppState;
pub use axum::extract::{Path, Query, State};
